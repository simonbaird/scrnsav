mod idle;
mod render;

use anyhow::Context;

/// Parsed options shared by the subcommands.
struct Opts {
    /// Idle timeout in seconds (first bare number), used by `watch`.
    secs: Option<u64>,
    /// Bundled effect name, `random`, or WGSL file path (`--shader NAME|PATH|random`).
    shader: Option<String>,
    /// Internal: set by the idle daemon on the `show` it spawns, so the saver
    /// dismisses on any input rather than Escape-only.
    idle: bool,
    /// Output PNG path (`--out PATH`), used by `shot`.
    out: Option<String>,
    /// Shot size as `WxH` (`--size 1920x1080`), used by `shot`.
    size: Option<String>,
    /// Animation time in seconds to capture (`--time SECS`), used by `shot`.
    time: Option<f32>,
    /// Phase-offset seed (`--seed F`); random each run when omitted. Used by `shot`.
    seed: Option<f32>,
    /// Print the bundled effect names and exit (`--list`).
    list: bool,
}

fn parse_opts(args: &[String]) -> Opts {
    let mut secs = None;
    let mut shader = None;
    let mut idle = false;
    let mut out = None;
    let mut size = None;
    let mut time = None;
    let mut seed = None;
    let mut list = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--shader" | "-s" => {
                shader = args.get(i + 1).cloned();
                i += 2;
            }
            "--out" | "-o" => {
                out = args.get(i + 1).cloned();
                i += 2;
            }
            "--size" => {
                size = args.get(i + 1).cloned();
                i += 2;
            }
            "--time" => {
                time = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--seed" => {
                seed = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--idle" => {
                idle = true;
                i += 1;
            }
            "--list" => {
                list = true;
                i += 1;
            }
            other => {
                if let Ok(n) = other.parse::<u64>() {
                    secs = Some(n);
                }
                i += 1;
            }
        }
    }
    Opts {
        secs,
        shader,
        idle,
        out,
        size,
        time,
        seed,
        list,
    }
}

/// Print the names of the bundled effects (the first is the default).
fn print_bundled() {
    println!("Bundled effects (first is the default):");
    for name in render::bundled_names() {
        println!("  {name}");
    }
}

/// Parse a `WxH` size string, defaulting to 1920x1080.
fn parse_size(size: Option<&str>) -> anyhow::Result<(u32, u32)> {
    let Some(s) = size else {
        return Ok((1920, 1080));
    };
    let (w, h) = s
        .split_once(['x', 'X'])
        .with_context(|| format!("invalid --size '{s}', expected WxH"))?;
    Ok((
        w.trim()
            .parse()
            .with_context(|| format!("invalid width in '{s}'"))?,
        h.trim()
            .parse()
            .with_context(|| format!("invalid height in '{s}'"))?,
    ))
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("show");
    let rest = args.get(1..).unwrap_or(&[]);

    match mode {
        "show" => {
            let opts = parse_opts(rest);
            if opts.list {
                print_bundled();
                return Ok(());
            }
            // Explicit run: Escape-only. Idle daemon passes --idle for any-input.
            let dismiss = if opts.idle {
                render::DismissMode::AnyInput
            } else {
                render::DismissMode::EscapeOnly
            };
            render::run(opts.shader, dismiss)?;
        }
        "watch" => {
            let opts = parse_opts(rest);
            let secs = opts.secs.unwrap_or(300);
            pollster::block_on(idle::run(secs * 1000, opts.shader))?;
        }
        "shot" => {
            let opts = parse_opts(rest);
            let out = opts.out.unwrap_or_else(|| "shot.png".to_string());
            let (w, h) = parse_size(opts.size.as_deref())?;
            // A moment into the animation, so the effect isn't caught at t=0.
            let time = opts.time.unwrap_or(10.0);
            render::shot(opts.shader, &out, w, h, time, opts.seed)?;
        }
        "list" | "--list" => print_bundled(),
        "-h" | "--help" | "help" => print_help(),
        other => {
            eprintln!("scrnsav: unknown command '{other}'\n");
            print_help();
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "scrnsav — a Wayland/GNOME screensaver\n\
         \n\
         USAGE:\n\
         \u{20}   scrnsav show  [--shader NAME|PATH|random]         Run the saver now (Esc to exit)\n\
         \u{20}   scrnsav watch [secs] [--shader NAME|PATH|random]  Watch for idle, then launch the saver\n\
         \u{20}   scrnsav shot  [--shader NAME|PATH|random] [--out PATH] [--size WxH] [--time SECS] [--seed F]\n\
         \u{20}                                                    Render one frame to a PNG (headless)\n\
         \u{20}   scrnsav list                                     List the bundled effect names\n\
         \n\
         --shader takes a bundled name (see `list`), a path to a .wgsl file,\n\
         or `random` to pick a bundled effect on each launch (repeats possible).\n\
         Without it, the default bundled effect is used. Default idle is 300s.\n\
         shot defaults: --out shot.png --size 1920x1080 --time 10, random --seed.\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_secs_and_shader() {
        let args = [
            "60".to_string(),
            "--shader".to_string(),
            "x.wgsl".to_string(),
        ];
        let opts = parse_opts(&args);
        assert_eq!(opts.secs, Some(60));
        assert_eq!(opts.shader.as_deref(), Some("x.wgsl"));
    }

    #[test]
    fn parses_shot_size() {
        assert_eq!(parse_size(None).unwrap(), (1920, 1080));
        assert_eq!(parse_size(Some("800x600")).unwrap(), (800, 600));
    }
}
