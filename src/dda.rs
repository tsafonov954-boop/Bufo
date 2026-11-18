//! DXGI Desktop Duplication capture.
//!
//! Windows Graphics Capture only gets frames when the desktop compositor
//! redraws, and a fullscreen game that flips straight to the screen bypasses it
//! (measured: ~0 frames/s from a 112 FPS game unless the cursor moved).
//! Desktop Duplication reads what is really sent to the monitor, so it works
//! for those games. The newest frame is kept on the GPU and handed to the
//! hardware encoder as a texture at a constant frame rate; nothing is copied
//! through the CPU.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    core::Interface,
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
                D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
            },
            Dxgi::{
                CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
                IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
                DXGI_OUTDUPL_FRAME_INFO,
            },
        },
    },
};

use crate::wgc::{EncCfg, Recorder};

/// Textures in flight between us and the encoder.
const RING: usize = 8;

pub struct Control {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Control {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Dup {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dupl: IDXGIOutputDuplication,
    width: u32,
    height: u32,
}

/// Find the adapter output that is the primary monitor (virtual display
/// adapters are skipped because they have no output at 0,0).
fn open_primary() -> Result<Dup, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| e.to_string())?;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let mut j = 0;
            while let Ok(output) = adapter.EnumOutputs(j) {
                j += 1;
                let Ok(desc) = output.GetDesc() else { continue };
                let r = desc.DesktopCoordinates;
                if !desc.AttachedToDesktop.as_bool() || r.left != 0 || r.top != 0 {
                    continue;
                }
                return open_output(&adapter, &output, (r.right - r.left) as u32, (r.bottom - r.top) as u32);
            }
        }
    }
    Err("no primary output found".into())
}

fn open_output(adapter: &IDXGIAdapter1, output: &IDXGIOutput, width: u32, height: u32) -> Result<Dup, String> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| e.to_string())?;
        let device = device.ok_or("no device")?;
        let ctx = ctx.ok_or("no context")?;
        // The encoder thread uses our textures concurrently with this thread.
        if let Ok(mt) = ctx.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let out1: IDXGIOutput1 = output.cast().map_err(|e| e.to_string())?;
        let dupl = out1.DuplicateOutput(&device).map_err(|e| e.to_string())?;
        Ok(Dup { device, ctx, dupl, width, height })
    }
}

pub fn start(
    dir: PathBuf,
    fps: u32,
    bitrate: u32,
    seg_secs: u64,
    audio_rx: Option<Receiver<Vec<u8>>>,
) -> Result<Control, String> {
    let probe = open_primary()?;
    let cfg = EncCfg { dir, width: probe.width, height: probe.height, fps, bitrate, audio: audio_rx.is_some() };
    drop(probe);
    let mut rec = Recorder::standalone(cfg, seg_secs, audio_rx)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let handle = thread::Builder::new()
        .name("dda".into())
        .spawn(move || {
            run(&mut rec, fps, &stop2);
            rec.close();
        })
        .map_err(|e| e.to_string())?;
    Ok(Control { stop, handle: Some(handle) })
}

/// GPU textures: the newest desktop frame, and a ring of copies given to the encoder.
struct Frames {
    latest: ID3D11Texture2D,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
}

fn make_frames(d: &Dup, src: &ID3D11Texture2D) -> Result<Frames, String> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        src.GetDesc(&mut desc);
        desc.MiscFlags = 0;
        desc.CPUAccessFlags = 0;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        let mut make = || -> Result<ID3D11Texture2D, String> {
            let mut t = None;
            d.device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(|e| e.to_string())?;
            t.ok_or_else(|| "no texture".to_string())
        };
        let latest = make()?;
        let ring = (0..RING).map(|_| make()).collect::<Result<Vec<_>, _>>()?;
        Ok(Frames { latest, ring, next: 0 })
    }
}

fn run(rec: &mut Recorder, fps: u32, stop: &AtomicBool) {
    let tick_ns = 1_000_000_000u64 / fps.max(1) as u64;
    let t0 = Instant::now();
    let due_at = |n: u64| t0 + Duration::from_nanos(tick_ns * n);
    let mut n: u64 = 0;

    'outer: while !stop.load(Ordering::Relaxed) {
        let d = match open_primary() {
            Ok(d) => d,
            Err(e) => {
                crate::settings::log(&format!("dda: cannot open output: {e}"));
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let mut frames: Option<Frames> = None;
        let mut have_frame = false;

        while !stop.load(Ordering::Relaxed) {
            let wait = due_at(n).saturating_duration_since(Instant::now());
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            let r = unsafe { d.dupl.AcquireNextFrame(wait.as_millis() as u32, &mut info, &mut res) };
            match r {
                Ok(()) => {
                    if let Some(tex) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                        if frames.is_none() {
                            match make_frames(&d, &tex) {
                                Ok(f) => frames = Some(f),
                                Err(e) => crate::settings::log(&format!("dda: textures: {e}")),
                            }
                        }
                        if let Some(f) = &frames {
                            if info.LastPresentTime != 0 || !have_frame {
                                unsafe { d.ctx.CopyResource(&f.latest, &tex) };
                                have_frame = true;
                            }
                        }
                    }
                    unsafe {
                        let _ = d.dupl.ReleaseFrame();
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    crate::settings::log("dda: access lost, reopening");
                    continue 'outer;
                }
                Err(e) => {
                    crate::settings::log(&format!("dda: acquire failed: {e}"));
                    thread::sleep(Duration::from_millis(200));
                    continue 'outer;
                }
            }

            if Instant::now() >= due_at(n) {
                if let (Some(f), true) = (frames.as_mut(), have_frame) {
                    let slot = &f.ring[f.next];
                    f.next = (f.next + 1) % RING;
                    unsafe {
                        d.ctx.CopyResource(slot, &f.latest);
                        d.ctx.Flush();
                    }
                    let ts = (t0.elapsed().as_nanos() / 100) as i64;
                    rec.push_texture(slot, ts);
                }
                n += 1;
                // Never try to catch up after a stall: skip missed ticks.
                let behind = (t0.elapsed().as_nanos() / tick_ns as u128) as u64;
                if behind > n + 2 {
                    n = behind;
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            break 'outer;
        }
    }
}
