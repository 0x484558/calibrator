# Calibrator

This lightweight background application applies automatic HDR brightness value calibration following laptop panel brightness adjustment, following Gamma 2.4 curve.

## Rationale and Limitations

Window has apparently implemented a coherent piece of mathematics and then hidden it behind two unrelated percentage sliders, lest users become dangerously informed: panel brightness and HDR content brightness. At first, it seems nigh impossible to determine correct values for both to ensure color accuracy of HDR content, while simultaneously it is clear that they are not intended to be tuned perceptually either, contrary to the UX intent. Because changing display brightness does not just cut peak values but squishes the entire brightness curve, so white point has to also change non-linearly with brightness to preserve dynamic range. Even more confusing is the fact that Windows HDR calibration app seems to be affected by HDR brightness slider.

I've collected a range of data points on my well-behaved 1100 nits OLED HDR laptop panel, and found that the relationship is unexpectedly regular. On the panel-brightness interval between roughly 42% and 98%, the **HDR content brightness** value required to make the Windows HDR Calibration app clip at exactly 1100 nits is extremely well approximated by a normalized power curve with exponent 2.4:

```math
f(b)
=
100
\frac{b^{2.4}-42^{2.4}}
     {98^{2.4}-42^{2.4}}
```

where $b$ is the ordinary panel-brightness percentage and $f(b)$ is the corresponding HDR-content-brightness percentage. The measured points agree with this approximation to within roughly one slider step.

**This calibration curve is restored through experimentation for my display and may not be correct on other panels.** Windows documents that integrated, nits-calibrated displays use a perceptually distributed mapping from brightness percentage to physical luminance, so a strongly nonlinear power-like relationship here, but exact 2.4 gamma exponent is just a single remarkably well-behaved panel; the accuracy of the curve and chosen exponent still can vary between panels. It is still possible that the curve will remain accurate for other well-calibrated ~1000-nits OLED panels.

The HDR-content-brightness control is exposed through `DISPLAYCONFIG_SDR_WHITE_LEVEL`. Windows encodes its 0–100 slider as an SDR white level according to

```math
S(h)=1000+50h,
```

and the documented fixed-point representation corresponds to

```math
W_{\mathrm{SDR}}(h)
=
80\frac{S(h)}{1000}
=
80+4h
\quad\mathrm{cd/m^2}.
```

Combining this with the measured compensation curve gives an approximate SDR-white luminance associated with each panel-brightness position:

```math
W_{\mathrm{SDR}}(b)
=
80+
400
\frac{b^{2.4}-42^{2.4}}
     {98^{2.4}-42^{2.4}}.
```

For example, this maps approximately:

- 42% panel brightness → 80 nits SDR white
- 47% → 99–100 nits
- 70% → 225 nits
- 80% → 303 nits
- 98% → 480 nits

**PRO TIP: Windows is solving a viewing-environment problem.** If you are familiar with content mastering, these values can be highly off-putting, and you would be right. **To attain ~203 nits white level, use 67% screen brightness value; to attain ~300 nits white level, use 80% screen brightness.** But keep in mind that said values are valid only for particular curve/exponent described here by default and assume a well-behaved panel.

Calibrator app, therefore, is reconstructing the nonlinear relationship between the panel's perceptually distributed brightness control and the SDR-white/HDR balance required to preserve the intended absolute HDR luminance scale. At least, for my display.

The useful calibrated domain is approximately $42\%\le b\le98\%$. Outside it, the required HDR-content-brightness value falls outside Windows' representable 0–100 range. In particular, extrapolation predicts roughly $f(100)\approx106$, while Windows can only apply 100; experimentally, 100/100 clips near 1050 nits instead of 1100. This is, nonetheless, accurate and faithful Windows behavior; there is no fundamental technical reason why Windows does not expose those 6 additional values.

Either way, the conclusion is that there exists a reproducible way to keep the panel on the same **peak-HDR calibration contour** while changing brightness within that interval. This preserves the observed 1100-nit clipping point, but it should be noted for disclosure that, by itself, this cannot be taken as proof that the entire PQ EOTF, white point, gamut, or other colorimetric properties remain perfectly calibrated at every point on the curve.

Thus, I developed this interactive-session Windows binary: it has no console, no visible top-level window, foreground activation, polling, recurring timer, network activity, power requests, or registry integration; the tray icon is its only integration surface, and only because necessary APIs are impossible to access from the Windows Service context. Normal launch applies a calibrated HDR balance after startup and real brightness changes, resume, unlock, monitor-device, display-power, or canonical GPU-TDR events. Of course, one-shot brightness changes are coalesced in 150 ms window. Each private SDR-white-level write occurs once and is then read once; mismatch/read failure will not cause a retry. No unique active internal HDR panel means no adjustment. Launching the executable twice signals existing instance to reapply calibration in case explicit refresh is necessary. `WM_DISPLAYCHANGE` broadcasts exclude message-only windows. Monitor PnP, display-power, resume, unlock, and GPU TDR events cover the permitted event-only shape, and every such event re-enumerates current active topology. The setter packet type `0xFFFFFFEE` is actually undocumented Windows API, which means no guarantees that this will work forever.

## Building

Native Windows builds use the pinned nightly toolchain:

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
```

Linux can produce the same MSVC-targeted PE binary with `cargo-xwin`. It uses LLD linker and downloads the Microsoft MSVC CRT and Windows SDK from Microsoft's official packages. Using those packages accepts the applicable Microsoft license. LLVM's resource compiler and Clang preprocessor are also required to embed the multi-resolution application icon (`sudo apt install llvm clang` on Debian/Ubuntu).

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --version 0.23.0 --locked
XWIN_ARCH=x86_64 cargo xwin build --release --locked \
    --target x86_64-pc-windows-msvc
```

The binary is written to `target/x86_64-pc-windows-msvc/release/calibrator.exe`. Set `XWIN_CACHE_DIR` to a persistent disk-backed directory when the default Cargo cache location is unsuitable; avoid memory-backed temporary filesystems because the SDK extraction requires substantial space.

## Probe mode (EXPERIMENT)

> Currently the app only uses built-in power curve with exponent 2.4. Probe mode will not change the hard-coded curve.

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
