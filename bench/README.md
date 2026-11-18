# Benchmark: Bufo vs other recorders

Goal: a fair, repeatable comparison anyone can re-run.

## Method
1. Same PC, same game/scene, same resolution, same in-game settings. Windowed or borderless, 1080p60 recording for every recorder; match bitrate (Bufo default 6 Mbit/s) and codec as closely as the other app allows.
2. For each recorder: start it in "instant replay / background recording" mode, wait until it is recording, then run `bench.ps1` for 120 s. Do 3 runs and take the median.
3. Also run a **baseline** with no recorder, to see what the recorder adds to the game's own CPU/GPU load.
4. FPS impact is measured separately with PresentMon or the game's own counter (not in this script); record average and 1% low FPS with and without the recorder.

## Run
```powershell
cd bench
.\bench.ps1 -Label baseline -Process none  -Seconds 120
.\bench.ps1 -Label bufo     -Process bufo  -Start ..\target\release\bufo.exe -Seconds 120
.\bench.ps1 -Label medal    -Process Medal -Seconds 120   # name from Task Manager, may need several: "Medal,Medal.Overlay"
```

## Reading the numbers
- Working set is what Task Manager shows; private bytes is memory only this process owns.
- CPU is percent of one core, summed over matching processes.
- GPU numbers are for the whole card (NVIDIA only); compare each run against the baseline.

## Limits
- Disk write speed and file size are not measured here (check the buffer folder size separately).
- Results depend on hardware and drivers; publish the PC spec with them.
