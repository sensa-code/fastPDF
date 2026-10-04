//! Minimal argument parsing with std only (spec §36: no clap for a handful
//! of flags).

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) const USAGE: &str = "\
fastpdf-bench — FastPDF benchmark harness

USAGE:
  fastpdf-bench open    <file.pdf>   [options]
  fastpdf-bench render  <file.pdf>   [--page N] [--tile PX] [--viewport WxH] [--workers N] [--out page.png]
  fastpdf-bench full    <file.pdf>   [options]
  fastpdf-bench corpus  <manifest.json|dir> [--out results.json] [--timeout SECS] [--repeat N]
  fastpdf-bench compare <baseline.json> <candidate.json> [--threshold PERCENT]
  fastpdf-bench engines

OPTIONS:
  --engine NAME      engine adapter (default: first compiled in; see `engines`)
  --scale S          display scale, 1.0 = 100% zoom at 96 dpi (default 1.0)
  --page N           1-based page for `render` (default 1)
  --tile PX          render as PX-sized tiles through the render scheduler
  --viewport WxH     with --tile: only tiles visible in a WxH view at the page top
  --workers N        with --tile: scheduler worker threads (default: auto)
  --repeat N         repetitions (open/render: in-process; corpus: child runs per file)
  --samples N        pages sampled for page-render / thumbnail stats in `full` (default 10)
  --password PW      password for encrypted files
  --out PATH         render: write PNG; corpus: write JSON results
  --timeout SECS     corpus: per-file timeout (default 120)
  --threshold PCT    compare: regression threshold in percent (default 10)
";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    Help,
    Engines,
    Open(PathBuf),
    Render(PathBuf),
    Full(PathBuf),
    Corpus(PathBuf),
    Compare {
        baseline: PathBuf,
        candidate: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Args {
    pub(crate) command: Command,
    pub(crate) engine: Option<String>,
    pub(crate) scale: f32,
    pub(crate) page: u32,
    pub(crate) tile: Option<u32>,
    pub(crate) viewport: Option<(u32, u32)>,
    pub(crate) workers: Option<usize>,
    pub(crate) repeat: u32,
    pub(crate) samples: usize,
    pub(crate) password: Option<String>,
    pub(crate) out: Option<PathBuf>,
    pub(crate) timeout_secs: u64,
    pub(crate) threshold: f64,
}

impl Args {
    fn new(command: Command) -> Self {
        Self {
            command,
            engine: None,
            scale: 1.0,
            page: 1,
            tile: None,
            viewport: None,
            workers: None,
            repeat: 1,
            samples: 10,
            password: None,
            out: None,
            timeout_secs: 120,
            threshold: 10.0,
        }
    }
}

pub(crate) fn parse(raw: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut it = raw.into_iter().map(|a| a.to_string_lossy().into_owned());
    let mut positional = Vec::new();
    let mut flags: Vec<(String, String)> = Vec::new();
    while let Some(arg) = it.next() {
        if arg == "-h" || arg == "--help" {
            return Ok(Args::new(Command::Help));
        }
        if let Some(name) = arg.strip_prefix("--") {
            let (name, value) = match name.split_once('=') {
                Some((n, v)) => (n.to_owned(), v.to_owned()),
                None => {
                    let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                    (name.to_owned(), value)
                }
            };
            flags.push((name, value));
        } else {
            positional.push(arg);
        }
    }

    let mut pos = positional.into_iter();
    let sub = pos.next().ok_or("missing command")?;
    let mut path = |what: &str| -> Result<PathBuf, String> {
        pos.next()
            .map(PathBuf::from)
            .ok_or_else(|| format!("`{sub}` needs {what}"))
    };
    let command = match sub.as_str() {
        "help" => Command::Help,
        "engines" => Command::Engines,
        "open" => Command::Open(path("a PDF file")?),
        "render" => Command::Render(path("a PDF file")?),
        "full" => Command::Full(path("a PDF file")?),
        "corpus" => Command::Corpus(path("a manifest or directory")?),
        "compare" => Command::Compare {
            baseline: path("a baseline JSON")?,
            candidate: path("a candidate JSON")?,
        },
        other => return Err(format!("unknown command `{other}`")),
    };
    if let Some(extra) = pos.next() {
        return Err(format!("unexpected argument `{extra}`"));
    }

    let mut args = Args::new(command);
    for (name, value) in flags {
        let bad = |e: &dyn std::fmt::Display| format!("--{name} {value}: {e}");
        match name.as_str() {
            "engine" => args.engine = Some(value),
            "scale" => {
                let s: f32 = value.parse().map_err(|e| bad(&e))?;
                if !(s.is_finite() && s > 0.0) {
                    return Err(bad(&"must be a positive number"));
                }
                args.scale = s;
            }
            "page" => {
                args.page = value.parse().map_err(|e| bad(&e))?;
                if args.page == 0 {
                    return Err(bad(&"pages are 1-based"));
                }
            }
            "tile" => args.tile = Some(value.parse().map_err(|e| bad(&e))?),
            "viewport" => {
                let (w, h) = value
                    .split_once(['x', 'X'])
                    .ok_or_else(|| bad(&"expected WxH"))?;
                args.viewport = Some((
                    w.parse().map_err(|e| bad(&e))?,
                    h.parse().map_err(|e| bad(&e))?,
                ));
            }
            "workers" => args.workers = Some(value.parse().map_err(|e| bad(&e))?),
            "repeat" => args.repeat = value.parse::<u32>().map_err(|e| bad(&e))?.max(1),
            "samples" => args.samples = value.parse().map_err(|e| bad(&e))?,
            "password" => args.password = Some(value),
            "out" => args.out = Some(PathBuf::from(value)),
            "timeout" => args.timeout_secs = value.parse().map_err(|e| bad(&e))?,
            "threshold" => args.threshold = value.parse().map_err(|e| bad(&e))?,
            _ => return Err(format!("unknown option --{name}")),
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(OsString::from))
    }

    #[test]
    fn parses_render_flags() {
        let a = p("render doc.pdf --page 3 --tile=256 --viewport 1920x1080 --workers 4").unwrap();
        assert_eq!(a.command, Command::Render("doc.pdf".into()));
        assert_eq!(
            (a.page, a.tile, a.viewport, a.workers),
            (3, Some(256), Some((1920, 1080)), Some(4))
        );
    }

    #[test]
    fn parses_compare() {
        let a = p("compare a.json b.json --threshold 5").unwrap();
        assert!(matches!(a.command, Command::Compare { .. }));
        assert_eq!(a.threshold, 5.0);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(p("").is_err());
        assert!(p("render").is_err());
        assert!(p("render a.pdf --page 0").is_err());
        assert!(p("render a.pdf --scale -1").is_err());
        assert!(p("render a.pdf --bogus 1").is_err());
        assert!(p("render a.pdf b.pdf").is_err());
        assert!(p("render a.pdf --page").is_err());
        assert_eq!(p("--help").unwrap().command, Command::Help);
    }
}
