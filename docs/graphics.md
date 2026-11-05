# Graphics profile

This document tells how Zwift selects its graphics profile, why `launch` measures the GPU, and where the limits of the measurement come from.

## How Zwift selects a profile

Zwift has four graphics profiles: basic, medium, high and ultra. Each profile is a text file in the install directory:

```text
data/configs/basic.txt
data/configs/medium.txt
data/configs/high.txt
data/configs/ultra.txt
```

At start, the game reads the name of the GPU and compares it with a table in `ZwiftApp.exe`. Each row of the table has a vendor, a part of a model name and a profile. The first row that matches selects the profile. The game then loads the one file with the name of that profile.

In game 1.124.0, the last rows of the table match the vendor only:

| Vendor | Profile |
| --- | --- |
| AMD | medium |
| NVIDIA | medium |
| ATI | basic |
| INTEL | basic |
| VMWARE | basic |

A GPU that matches no row gets basic. The table is older than many current GPUs. For example, an AMD Radeon 880M gets basic, but it runs at about 60 FPS with ultra.

The saved resolution of the user (`prefs.xml` in `Documents/Zwift`) overrides the `res` line of the profile file. So the profiles change mostly the shadows, the ambient occlusion (SSAO) and the level of detail.

## What launch does

The game reads only the file of the profile that its table selects, and `launch` cannot know that profile before the start. So `launch` writes the content of the profile that it selects into all four files.

1. For each of the four files, `launch` calculates the CRC-32. If the CRC is equal to the CRC in the local manifest, the file is the original, and `launch` copies it to `.zwift-bootstrap/profiles/<name>.txt`. After a game update brings a new original, the next launch updates the copy.
2. `launch` reads the setting in `.zwift-bootstrap/graphics`. `zwift-bootstrap graphics` writes this file. If there is no file, the setting is `auto`.
3. With `auto`, `launch` runs the GPU test and selects the profile from the result. With a profile name, `launch` uses that profile and runs no test.
4. `launch` writes the copy of the selected profile into all four files.

If the GPU test fails, or if there is no copy of the selected profile, `launch` writes each file back to its own original, where a copy exists. Then the table of the game selects the profile. `launch` continues to start the game in all of these cases.

`launch` writes one line on stderr, for example:

```text
graphics: ultra (GPU test 6.87 ms on AMD Radeon 880M Graphics (radeonsi, strix1, ACO, DRM 3.64, 7.2.11-arch1-1))
graphics: high (set with zwift-bootstrap graphics)
graphics: game default (GPU test failed: libEGL.so.1: cannot open shared object file: No such file or directory)
```

`verify` accepts a profile file that has the content of one of the four originals in the manifest. An update downloads only the files whose manifest entry changed, so an update can write back one original file. The next launch writes the selected profile again.

## Why launch measures the GPU

The speed of a laptop GPU changes with the power state. On the test laptop, the same GPU test took 6.87 ms at full power and 13.47 ms at a 10 W power limit, for example on battery or on a weak charger. A name table cannot see this. So `launch` measures the GPU immediately before each start.

## The GPU test

The test runs in the `launch` process before Wine starts, with the environment that the game gets. So a variable such as `DRI_PRIME` applies to the test and to the game.

The test makes a headless OpenGL 3.3 core context with EGL. It tries these EGL platforms in this order:

1. The Mesa surfaceless platform. On Mesa, it uses the GPU that `DRI_PRIME` selects.
2. Each EGL device, for drivers that do not have the surfaceless platform, for example the NVIDIA driver.

The test does not use a software renderer (llvmpipe, softpipe, swrast). The game cannot run well on a software renderer, and the test takes many seconds on it.

Each frame of the test does these steps:

1. Draw a mesh of 400 x 400 squares 4 times into a 2048 x 2048 depth texture (the shadow map).
2. Draw the same mesh 4 times into a 1920 x 1080 RGBA8 color texture with a 24-bit depth texture. Each pixel samples the shadow map 16 times.
3. Do 4 full-screen passes. Each pixel samples the depth texture 25 times and the color texture 24 times.

A `GL_TIME_ELAPSED` query measures the GPU time of each frame. The test draws 20 frames to warm up and then 40 measured frames. The result is the median of the 40 frame times. On the test laptop, the full test takes 0.4 to 0.9 seconds.

The limits below are correct only for this workload. If you change a shader, a size or a count in `src/gpu_test.rs`, you must calibrate the limits again.

## Calibration

The data comes from one laptop with an AMD Radeon 880M, at 1920 x 1080, with vsync off (`vblank_mode=0`). Each row is one ride of about 2 minutes. The riding FPS is the median of the `FPS` lines in `Log.txt` while the game is in `ZSF_RIDE`.

| Profile | Chip state | GPU test | Riding FPS |
| --- | --- | --- | --- |
| ultra | full power at start, no power logger | 6.87 ms | 61.5 |
| ultra | 10 W PPT, GPU 600 MHz | 13.47 ms | 42.8 |
| high | 10 W, 600 MHz | 13.53 ms | 44.8 |
| medium | 10 W, 600 MHz | 12.27 ms | 50.9 |
| basic | 10 W, 600 MHz | 12.75 ms | 52.0 |

The model is:

```text
Zwift frame time (ms) = 8.87 + k * GPU test time (ms)
```

The two ultra rows give the fixed part, 8.87 ms, and k for ultra. The other rows give k for their profile with the same fixed part:

| Profile | k | Limit |
| --- | --- | --- |
| ultra | 1.076 | 7.25 ms |
| high | 0.995 | 7.84 ms |
| medium | 0.879 | 8.88 ms |
| basic | 0.813 | none |

The limit of a profile is the GPU test time at which the model frame time is 16.67 ms (60 FPS):

```text
limit = (16.67 - 8.87) / k
```

A calculation from the rounded values in the tables can differ from these limits by up to 0.01 ms.

`launch` selects the highest profile whose limit is equal to or more than the GPU test time. If the test time is more than all limits, `launch` selects basic. The limits are in `MAX_FRAME_MS_FOR_60_FPS` in `src/gpu_test.rs`.

## Limits of the calibration

- All data comes from one machine.
- The fixed part of 8.87 ms is probably the CPU work of this laptop. On a different CPU, the fixed part is different.
- The limits for desktop computers with a dedicated GPU are not verified.

If you use `auto`, please report the GPU test value that `launch` writes, your GPU and CPU, and the profile that works well for you. Use the [hardware report](https://github.com/bpauli/zwift-bootstrap/issues/new?template=hardware-report.yml) template.
