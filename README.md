# Calibrator

This lightweight background application applies automatic HDR brightness value calibration following laptop panel brightness adjustment, following Gamma 2.4 curve.

## Rationale and Limitations

Window has apparently implemented a coherent piece of mathematics and then hidden it behind two unrelated percentage sliders, lest users become dangerously informed: panel brightness and HDR content brightness.

At first, it seems nigh impossible to determine correct values for both to ensure color accuracy of HDR content, while simultaneously it is clear that they are not intended to be tuned perceptually either contrary to seeming UX intent. Even more convoluted is the fact that Windows HDR calibration app seems to be affected by HDR brightness slider.

However, by toying around this "three-body problem" and collecting various data points on my well-behaved 1100 nits OLED HDR laptop panel, I revealed that it actually seems to have unexpectedly familiar curve. And in fact it does: on panel brightness interval between ~42% and ~98%, the content brightness value that achieves clipping at exactly 1100 nits in the HDR calibration app follows almost perfectly gamma 4.2 curve. Why, even the fact that above ~98% it maps to content brightness values reaching unattainable 106% is consistent with said curve! (and I bet having 6 additional integers would cause critical structural damage to the new Windows Settings app)

So I realized that there, in fact, exists a legitimately optimal way to reconfigure content brightness based on brightness of a well-behaved, hardware-linear laptop panel. I suppose the fact we have both sliders is a deliberate choice by Microsoft due to panel brightness curves being OEM-defined and hard to trust, and informing users about this would naturally border on defamation.

Thus, I developed this interactive-session Windows binary: it has no console, no visible top-level window, foreground activation, polling, recurring timer, network activitz, power requests, or registry integration; the tray icon is its only integration surface, and only because necessary APIs are nigh-impossible to access from the Windows Service context. Normal launch applies a calibrated HDR balance after startup and real brightness changes, resume, unlock, monitor-device, display-power, or canonical GPU-TDR events.

Of course, one-shot brightness changes are coalesced in 150 ms window. Each private SDR-white-level write occurs once and is then read once; mismatch/read failure will not cause a retry. No unique active internal HDR panel means no adjustment. Launching the executable twice signals existing instance to reapply calibration in case explicit refresh is necessary.

`WM_DISPLAYCHANGE` broadcasts exclude message-only windows. Monitor PnP, display-power, resume, unlock, and GPU TDR events cover the permitted event-only shape, and every such event re-enumerates current active topology. The setter packet type `0xFFFFFFEE` is actually undocumented Windows API, which means no guarantees that this will work forever (just use Linux)

## Probe mode (EXPERIMENT)

> Currently the app only uses built-in gamam 2.4 curve. Probe mode will not change the hard-coded curve.

Exit any running normal instance, then launch:

```powershell
calibrator.exe --probe
```

Default positions are `0,25,50,75,100`. A custom strictly increasing sequence is accepted:

```powershell
calibrator.exe --probe=0,10,30,50,70,90,100
```

For the position shown in the tray tooltip, manually set Windows **HDR/SDR brightness balance**, then right-click the tray icon and select **Record current value**. Repeat. Probe mode never invokes the private setter; all automatic adjustment paths remain suppressed. Each explicit capture performs one fresh topology/HDR check and one documented getter call. Unavailable, disabled, or ambiguous internal HDR targets are logged without advancing the requested position.

The log is:

```text
%LOCALAPPDATA%\Calibrator\hdr-sdr-white-level-probe.csv
```

`raw_sdr_white_level` is the observed public value. `expected_sdr_white_level` is the current encoding hypothesis `1000 + 50 × slider_position`; `encoding_matches` validates it per sample.

## Attribution and License

Coparight © [Vladyslav "Hex" Yamkovyi](https://0x484558.dev/), 2026.

Calibrator is licensed under the European Union Public License (EUPL) version 1.2; see [LICENSE](LICENSE) file for more info.
