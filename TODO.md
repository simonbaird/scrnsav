# scrnsav — ideas / backlog

## Shader system
- **Per-shader parameters** (`--balls 5`, etc.). WGSL uniform layouts are fixed,
  so this needs a scheme: e.g. a small fixed array of generic `param[]` floats in
  the Uniforms block, plus a per-shader manifest naming them — or drive it from a
  config file. Connects to the config-file and multiple-shaders items.
- **Shader directory discovery.** Also load user shaders from
  `$XDG_DATA_HOME/scrnsav/shaders/` so people can drop files in without full paths.
- **Hot-reload while authoring.** Watch the shader file and rebuild the pipeline
  on change (pairs well with the validation error-scope handling already in place).
- **More uniforms.** Frame count, delta time, per-launch random seed, maybe
  pointer position. Keep the `Uniforms` size test in step with any additions.

## Daemon / behaviour
- **Cycle mode.** Rotate through bundled shaders each time the daemon fires
  (random selection is available via `--shader random`).
- **Robust watch-mode errors.** Detect when the spawned `show` exits non-zero (bad
  shader, GPU init failure) and log the reason / fall back to the default, instead
  of just relaunching.
- **Monitor hotplug.** Monitors are enumerated once at launch; handle add/remove
  during a running session.
- **Respect inhibitors + power.** Don't fire during video playback / presentations
  (idle inhibit), and consider blanking the display (DPMS) after a longer secondary
  timeout to save power.
- **Fade slowly to black on trigger.** Gnome does this, and it's nice because
  you can twitch the mouse if you're actually not wanting the screensaver.

## Packaging & distribution
- **RPM + COPR.** Spec installs the binary to `/usr/bin`, shaders to
  `/usr/share/scrnsav/shaders`, and the systemd user unit to
  `/usr/lib/systemd/user`. Note: the current `make install` points the unit at the
  dev build dir, so packaging depends on the shader-directory-discovery item for
  fixed system paths.
- **CI.** GitHub Actions running `cargo fmt --check`, `cargo clippy`, and
  `cargo test` (which already validates every shader). Cheap, given the tests exist.
- **Config file.** `~/.config/scrnsav/config.toml` for timeout + shader +
  per-shader params, so the systemd unit can stay generic. Ties the shader-params
  and multiple-shaders items together.
- **README screenshots / GIFs.** Render effects headlessly to PNG (offscreen wgpu)
  for the README, and reuse as CI artifacts.

## Bigger / stretch
- **True lock integration on GNOME.** The only real route is a GNOME Shell
  extension (GJS/Clutter) that owns the lock visual — a separate, GNOME-version-
  coupled project. (This is the documented ceiling of the standalone approach.)
- **Portability to other compositors.** KDE / wlroots via `ext-idle-notify-v1`
  (+ `ext-session-lock-v1` where actually supported), broadening beyond GNOME.
- **Audio-reactive shaders.** Feed an FFT of the default audio sink in as a uniform.
- **Polish.** `--version`, a man page.

## Naming
- **Pick a real name** (`scrnsav` is a placeholder — vowel-less, awkward to say).
  It's also the binary name, so favour short/lowercase/typeable. Front-runners:
  `limn` (verb: to draw in outline — matches the default vector-lines effect) and
  `reverie` (the idle machine daydreams). `phosphene` is taken by a Mac screensaver
  (github.com/kageroumado/phosphene), so avoid the collision. Check GitHub +
  crates.io availability before committing to one; renaming now is cheap.
