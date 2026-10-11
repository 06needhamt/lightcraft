//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json; flate2/brotli for the web bundle). External tools (`cargo`, `curl`, `tar`) are
//! invoked through `std::process::Command`.

mod assets;
mod bench;
mod ico;
mod layers;
mod parity;
mod stats;
mod version;
mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  assets          every image/icon/font/media file is attributed in assets/ATTRIBUTION.md; no Adobe assets
  bench [FILE] [--strict] [--threshold PCT]
                  run the render benchmark, append to target/bench/history.jsonl, compare CPU time with
                  the previous run (default input: corpus/raw/arw-sony-a7m3-compressed.arw)
  ico <out.ico> <in.png>...
                  pack square PNGs (<= 256 px) into a Windows .ico (see packaging/icons.sh)
  layers          enforce the crate dependency layering (plan/architecture.md §3)
  parity [--write]
                  check docs/parity-checklist.md (every cmd:/ctl: id and path it cites exists) and print the
                  Lightroom parity summary; --write refreshes the summary table in the document
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates (+ the web app)
  web [--serve [port]] [--dev]
                  build the browser app (apps/lightcraft-web) into <target>/web-site/;
                  --serve serves it on http://127.0.0.1:<port> (default 8080)
  ci              fmt --check, clippy -D warnings, heif, test, parity refs, layers, assets, wasm (stops at first failure)
  corpus [--download]
                  show where test corpora live; --download fetches PngSuite and CC0 raw samples (raw.pixls.us) into corpus/ and checks their sha256
  stats [--exact] count tests and lines per crate (--exact: ask the test harness via `-- --list`)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let result = match args.first().map(String::as_str) {
        Some("ico") => ico::run(&rest),
        Some("version") => version::run(&root(), &rest),
        Some("layers") => cmd_layers(),
        Some("assets") => assets::run(&root()),
        Some("bench") => bench::run(&root(), &rest),
        Some("parity") => parity::run(&root(), rest.contains(&"--write")),
        Some("wasm") => cmd_wasm(),
        Some("web") => web::run(&rest),
        Some("ci") => cmd_ci(),
        Some("corpus") => cmd_corpus(rest.contains(&"--download")),
        Some("stats") => stats::run(&root(), rest.contains(&"--exact")),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Workspace root (parent of the xtask crate).
pub fn root() -> PathBuf {
    // `cargo run` sets it at run time; prefer that over the compile-time value, which goes stale
    // when the checkout moves and the cached xtask binary isn't rebuilt
    let dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from).filter(|d| d.join("Cargo.toml").is_file());
    let dir = dir.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    dir.parent().expect("xtask has a parent dir").to_path_buf()
}

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    // Full Windows debuginfo plus one linker per CPU can exhaust RAM before any
    // tests run. Scope these overridable defaults to CI, including parity/WASM
    // subprocesses, without changing ordinary developer builds or GPU coverage.
    if std::env::args().nth(1).as_deref() == Some("ci") {
        static JOBS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        let (build, threads) = JOBS.get_or_init(|| {
            let ram_mb = available_ram_mb().or_else(|| total_ram_gb().map(|gb| gb.saturating_mul(1024) / 2));
            let jobs = ci_jobs(ram_mb, std::thread::available_parallelism().map_or(4, |n| n.get()));
            // More test threads barely shorten CI (a few heavy tests dominate) but make the
            // wall-clock frame-budget tests flaky on a loaded machine.
            (jobs.to_string(), jobs.min(4).to_string())
        });
        for (key, value) in
            [("CARGO_PROFILE_DEV_DEBUG", "line-tables-only"), ("CARGO_BUILD_JOBS", build.as_str()), ("RUST_TEST_THREADS", threads.as_str())]
        {
            if std::env::var_os(key).is_none() {
                c.env(key, value);
            }
        }
    }
    c
}

/// CI build jobs: one per 1.5 GB of RAM available when CI starts, at most one per CPU, 4 when it
/// is unknown. With line-table debuginfo, 4 jobs measured about 3 GB of RAM in use and 2 jobs
/// about 1.5 GB, so this leaves about half the available RAM to spare. Counting available rather
/// than installed RAM keeps a busy machine (browsers, VMs, editors) from being overcommitted.
/// Test threads are this, capped at 4.
fn ci_jobs(available_mb: Option<u64>, cpus: usize) -> usize {
    available_mb.map_or(4, |mb| usize::try_from(mb / 1536).unwrap_or(usize::MAX)).clamp(1, cpus.max(1))
}

/// RAM the OS could hand out now (free plus reclaimable cache) in MB, if it reports it.
fn available_ram_mb() -> Option<u64> {
    let output = |program: &str, args: &[&str]| {
        let out = Command::new(program).args(args).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if cfg!(target_os = "linux") {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb = info.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?.trim().trim_end_matches("kB").trim();
        Some(kb.parse::<u64>().ok()? / 1024)
    } else if cfg!(windows) {
        let cmd = "(Get-CimInstance Win32_PerfFormattedData_PerfOS_Memory).AvailableMBytes";
        output("powershell", &["-NoProfile", "-Command", cmd])?.trim().parse().ok()
    } else {
        // macOS: free + inactive + speculative pages
        let stat = output("vm_stat", &[])?;
        let page: u64 = stat.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
        let pages = |name: &str| -> Option<u64> {
            let line = stat.lines().find(|l| l.starts_with(name))?;
            line.rsplit(':').next()?.trim().trim_end_matches('.').parse().ok()
        };
        let free = pages("Pages free")?.saturating_add(pages("Pages inactive")?).saturating_add(pages("Pages speculative").unwrap_or(0));
        Some(free.saturating_mul(page) >> 20)
    }
}

/// Installed physical memory in whole GB, if the OS reports it (the fallback when available RAM
/// can't be read: half of it is assumed available).
fn total_ram_gb() -> Option<u64> {
    let bytes: u64 = if cfg!(target_os = "linux") {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim();
        kb.parse::<u64>().ok()?.checked_mul(1024)?
    } else {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("powershell", &["-NoProfile", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
        } else {
            ("sysctl", &["-n", "hw.memsize"])
        };
        let out = Command::new(program).args(args).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()?
    };
    // round to the nearest GB: "8 GB" machines report slightly less
    Some(bytes.saturating_add(1 << 29) >> 30)
}

pub fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    eprintln!("$ {what}");
    let status = cmd.status().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what}: exited with {status}")) }
}

pub fn metadata() -> Result<serde_json::Value, String> {
    let out = cargo().args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))
}

fn cmd_layers() -> Result<(), String> {
    let crates = layers::from_metadata(&metadata()?)?;
    println!("Dependency layering (plan/architecture.md §3)\n");
    println!("{:<28} {:<14} workspace deps", "crate", "layer");
    for c in &crates {
        let ws: Vec<String> = c
            .deps
            .iter()
            .filter(|d| d.workspace)
            .map(|d| {
                let k = match d.kind {
                    layers::DepKind::Normal => "",
                    layers::DepKind::Dev => " (dev)",
                    layers::DepKind::Build => " (build)",
                };
                format!("{}{k}", layers::short_name(&d.name))
            })
            .collect();
        println!("{:<28} {:<14} {}", c.name, layers::describe(layers::classify(&c.name)), ws.join(", "));
    }
    let violations = layers::check(&crates);
    println!();
    if violations.is_empty() {
        println!("OK: {} crates, no layering violations.", crates.len());
        Ok(())
    } else {
        println!("{} violation(s):", violations.len());
        for v in &violations {
            println!("  - {v}");
        }
        Err(format!("{} layering violation(s)", violations.len()))
    }
}

/// Workspace packages that must build for wasm32: all L0–L5 crates plus
/// the egui shell and the web app.
fn wasm_set() -> Result<Vec<String>, String> {
    let crates = layers::from_metadata(&metadata()?)?;
    Ok(crates
        .into_iter()
        .filter(|c| match layers::classify(&c.name) {
            Some(layers::Class::Layer(l)) => l <= 5,
            Some(layers::Class::Standalone) => true,
            _ => false,
        })
        .map(|c| c.name)
        .chain(std::iter::once("lightcraft-web".to_string()))
        .collect())
}

fn cmd_wasm() -> Result<(), String> {
    let set = wasm_set()?;
    let mut results = Vec::new();
    for pkg in &set {
        let mut c = cargo();
        c.args(["check", "--target", "wasm32-unknown-unknown", "-p", pkg]);
        let ok = run(c, &format!("cargo check --target wasm32-unknown-unknown -p {pkg}")).is_ok();
        results.push((pkg.clone(), ok));
    }
    println!("\nwasm32-unknown-unknown check:");
    for (p, ok) in &results {
        println!("  {:<6} {p}", if *ok { "ok" } else { "FAIL" });
    }
    let failed = results.iter().filter(|r| !r.1).count();
    if failed == 0 { Ok(()) } else { Err(format!("{failed} crate(s) failed the wasm check")) }
}

fn cmd_ci() -> Result<(), String> {
    type Step = (&'static str, Box<dyn Fn() -> Result<(), String>>);
    let steps: Vec<Step> = vec![
        (
            "fmt",
            Box::new(|| {
                let mut c = cargo();
                c.args(["fmt", "--all", "--", "--check"]);
                run(c, "cargo fmt --all -- --check")
            }),
        ),
        (
            "clippy",
            Box::new(|| {
                let mut c = cargo();
                c.args(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy --workspace --all-targets -- -D warnings")
            }),
        ),
        (
            "heif",
            Box::new(|| {
                // the optional HEIC/HEIF decoder is off in the workspace build above
                let mut c = cargo();
                c.args(["clippy", "-p", "lightcraft-codecs", "--features", "heif", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy -p lightcraft-codecs --features heif --all-targets -- -D warnings")?;
                let mut c = cargo();
                c.args(["test", "-p", "lightcraft-codecs", "-p", "lightcraft-heif", "--features", "lightcraft-codecs/heif"]);
                run(c, "cargo test -p lightcraft-codecs -p lightcraft-heif --features lightcraft-codecs/heif")?;
                // the engine's HEIC import test, with the decoder (the workspace run checks the error)
                let mut c = cargo();
                c.args(["test", "-p", "lightcraft-engine", "--lib", "--features", "lightcraft-codecs/heif", "heic"]);
                run(c, "cargo test -p lightcraft-engine --lib --features lightcraft-codecs/heif heic")
            }),
        ),
        (
            "test",
            Box::new(|| {
                let mut c = cargo();
                c.args(["test", "--workspace"]);
                run(c, "cargo test --workspace")
            }),
        ),
        ("parity", Box::new(|| parity::run(&root(), false))),
        ("layers", Box::new(cmd_layers)),
        ("assets", Box::new(|| assets::run(&root()))),
        ("wasm", Box::new(cmd_wasm)),
    ];
    let mut done = Vec::new();
    for (name, f) in &steps {
        eprintln!("\n=== ci: {name} ===");
        if let Err(e) = f() {
            println!("\nCI summary:");
            for d in &done {
                println!("  ok    {d}");
            }
            println!("  FAIL  {name}: {e}");
            for (n, _) in steps.iter().skip(done.len() + 1) {
                println!("  skip  {n}");
            }
            return Err(format!("ci failed at `{name}`"));
        }
        done.push(*name);
    }
    println!("\nCI summary: all {} steps passed ({})", done.len(), done.join(", "));
    Ok(())
}

/// CC0 raw samples (raw.pixls.us). Small, representative set; extend freely (CC0 only).
/// CC0 raw samples from raw.pixls.us (each verified CC0 on the site).
/// The third field is the SHA-256 of the file as downloaded; `cargo xtask corpus --download` checks it.
/// One per format / compression variant we decode or deliberately report as unsupported (preview only).
const RAW_SAMPLES: &[(&str, &str, &str)] = &[
    (
        "arw-sony-a7rm4a-compressed.arw",
        "https://raw.pixls.us/getfile.php/4822/nice/Sony%20-%20ILCE-7RM4A%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "690c774f1d7bc1db3fa8c2489762743d8e7586e50b6f65d0b6a61cb467972c67",
    ),
    (
        "arw-sony-a9m2-compressed.arw",
        "https://raw.pixls.us/getfile.php/3989/nice/Sony%20-%20ILCE-9M2%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "161c2a9da2b5f1e50be6117a0b4da0ce249660b5d7de13568d301c4034716b97",
    ),
    (
        "arw-sony-a7m3-compressed.arw",
        "https://raw.pixls.us/getfile.php/2414/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "250784580ea527442c09004417bb0eead484f2bf3ee8f9121a776ac65bb50d0f",
    ),
    (
        "arw-sony-a7m3-uncompressed.arw",
        "https://raw.pixls.us/getfile.php/2418/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.ARW",
        "ece80551abf64949dbe826985a80f1d9b265401ee4ea6b989e1829549616fdcd",
    ),
    (
        "arw-sony-a7m4-14bit.arw",
        "https://raw.pixls.us/getfile.php/6936/nice/Sony%20-%20ILCE-7M4%20-%2014bit%20%283:2%29.ARW",
        "639a6d4db881f1359e3ea7e1137b314e59417d24e6d4d862b7202111a2c823b2",
    ),
    // lossless compressed (Compression 7): L is 2×2 CFA cells per LJ92 sample; M and S are subsampled
    (
        "arw-sony-a7m4-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW",
        "851b43c2116c4139104a5036f83ac3b6a148789b2142214dd7192c13972b25b6",
    ),
    (
        "arw-sony-a7m4-lossless-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06675_FullFrame-LossLess-Compressed-Medium.ARW",
        "d453005714327addd75bcb99c1c6223173dc92f2a59542d82e6761ef0e0e7571",
    ),
    (
        "arw-sony-a7m4-lossless-s.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06676_FullFrame-LossLess-Compressed-Small.ARW",
        "cbbd0930c7d8706dff84c68a2004454266e6fd0d8354f5f76a106b5d776e0223",
    ),
    // ILCE-7CR (61 MP), one scene in three codings: lossless compressed L (2×2 CFA cells per LJ92 sample) and M
    // (subsampled YCbCr), and compressed ARW2
    (
        "arw-sony-a7cr-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00795.ARW",
        "3e1642f3a1ae7c9f93228c5f09f27e362608e6a4ca1b3e5d8dd9eb8fae576997",
    ),
    (
        "arw-sony-a7cr-lossless-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00796.ARW",
        "5bcba4acc52a5b902074581b4a2f5fb3c79ec56bd0fdc1746b9e2c0f9d3e9c88",
    ),
    (
        "arw-sony-a7cr-compressed.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00798.ARW",
        "f5096e8fbccf0842c8a57763cabd2836608f019eb7fd54f7b22260c2f5bc4da5",
    ),
    // ILCE-7CM2 (33 MP, issue #697): lossless compressed L, and the compressed "M-size" coding, which is Bayer at
    // 4736×3132 with black 512 (only the lossless M/S codings are YCbCr with the 1024 pedestal)
    (
        "arw-sony-a7cm2-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CM2/DSC02431.ARW",
        "342d893bd71fd050c273df181e3039d3f12d88947b0662de5de5325c84cc449b",
    ),
    (
        "arw-sony-a7cm2-compressed-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CM2/DSC02436.ARW",
        "002c86d38a16b08a2438780728b4fda8efcc745fa6c26231dad6af67b383b11e",
    ),
    // a dusk sky clipped in green behind a poplar: the clipped-highlight colour of issue #523
    (
        "arw-sony-a7rm4-14bit-compressed.arw",
        "https://raw.pixls.us/getfile.php/3480/nice/Sony%20-%20ILCE-7RM4%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "e6dafe42643f69ab9d1fd00414b7a1f5104df354bb589201600ac934b29b5e4a",
    ),
    // pre-2017 bodies: white balance and black level only in the encrypted SR2SubIFD (#148)
    (
        "arw-sony-rx100m3.arw",
        "https://raw.pixls.us/data/Sony/DSC-RX100M3/DSC00734.ARW",
        "cb81392e3a8810231ce341fb45de3e4d1b9e2f87bc47f3c314d084041ccfd138",
    ),
    (
        "arw-sony-rx100.arw",
        "https://raw.pixls.us/data/Sony/DSC-RX100/DSC00838.ARW",
        "579a485b5126a25cbd55cbd5dadfa7d09cf021c99cc7d4869f9e56e3f759390b",
    ),
    (
        "arw-sony-a7rm2-12bit-uncompressed.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7RM2/12-bit-uncompressed.ARW",
        "71e0888396ef52c7e6a990b4e72ebb4d8413a1fc1c35bada449870539cef6e39",
    ),
    // older DSLRs whose SR2SubIFD keeps the black level elsewhere than the bodies above (#535): A500 (27152-byte
    // layout, like the A450/A550), A700 (62112 bytes)
    (
        "arw-sony-a500.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A500/DSC02421.ARW",
        "1407fb596a391df67c15b90026a1a76702a2e7386dbdff2f0815b903ea49cead",
    ),
    (
        "arw-sony-a700.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A700/DSC07249.ARW",
        "3159e28892bf0771d01525cd4b6190f9c15bbb19fa2fab6d2515ced0594e8f40",
    ),
    // SR2SubIFD white balance (#535): the A500 and A700 above, a third layout of a body that had none before
    // (SLT-A33, 29000 bytes), and two shots whose maker-note gains differ from the preset applied (5600 K, Shade)
    ("arw-sony-a33.arw", "https://raw.pixls.us/data/Sony/SLT-A33/DSC01867.ARW", "1a59856394f10d4fadb40f5ab9c6d1c89f473f4dbcdefd216fbbbbf1ad8d21f9"),
    (
        "arw-sony-a3500-5600k.arw",
        "https://raw.pixls.us/data/Sony/ILCE-3500/DSC06923.ARW",
        "04c4fe04425c3e6fff7c8af0f811923c35e750300373357bd54342150c5505d8",
    ),
    (
        "arw-sony-a7s-shade.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7S/DSC04125.ARW",
        "7cc338a0abc8fdad32d61006f1f8f412297b93e040c7efba572eb2bd8f8e8be2",
    ),
    // pre-2017 bodies in the camera's 16:9 mode (#535): the ILCE-7SM2 records it in FullImageSize, the DSLR-A580 only
    // in the Exif image size
    (
        "arw-sony-a7sm2-16x9.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7SM2/DSC01005.ARW",
        "46044fb6a9c805f2b4915cd4970316b4d04b0566bcf5287afaad3ef648d7a3db",
    ),
    (
        "arw-sony-a580-16x9.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A580/RAW_SONY_A580.ARW",
        "5b0924d39151239dce19e92e08318f4f62a5a8ac276bd7180ec8463d2cfee709",
    ),
    // packed 12-bit ARW (two pixels per three bytes): the DSLR-A900
    (
        "arw-sony-a900-packed12.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A900/_DSC7969.ARW",
        "ac7c1532df77c321e8010aa1be60c1b9f245b1ac0db6f38fe617a73aae81af50",
    ),
    // CR2 colour-filter layouts differ by model (issue #85): CR2CFAPattern 3 (GBRG) and 1 (RGGB) samples
    (
        "cr2-canon-40d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%2040D/_MG_0153.CR2",
        "775c806358fedddec7622113a5e399a3330bd5a65e35e37ae1108d8bf58067be",
    ),
    (
        "cr2-canon-550d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%20550D/IMG_4047.CR2",
        "f390e0ba566f0be9af2d8e7f955b815996630c0c3cb13fcfb95d98baec1890f6",
    ),
    (
        "cr2-canon-5d2.cr2",
        "https://raw.pixls.us/data/Canon/EOS%205D%20Mark%20II/08.canon.raw.cr2",
        "12b3ad6aed9fbcd19ba3bfffcd0769aa5a041d610205eee840bf4b2c420dffae",
    ),
    (
        "cr2-canon-5dsr.cr2",
        "https://raw.pixls.us/data/Canon/EOS%205DS%20R/_DSR2002.CR2",
        "01b94b0586419d64894ccdf2e7b9f1defeedc7b51dfcd306c622fe784181a9b8",
    ),
    (
        "cr2-canon-6d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%206D/EOS_6D_RAW.CR2",
        "360842779d08fe4805b4a2fe4979f06fdc6da982c85c3acb181f25bcf4c9f536",
    ),
    (
        "cr2-canon-7d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%207D/RAW_CANON_EOS_7D-raw.CR2",
        "b5e47c5fcf7332ac03e0134926f17a338a42e68c1fd7f83e16f45f4b767544e8",
    ),
    (
        "cr2-canon-5d3-sraw2.cr2",
        "https://raw.pixls.us/getfile.php/773/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%20sRAW2%20%28sRAW%29.CR2",
        "a79065e78a6c5bdbc8726e786ade02d54a7d496cc3cb4504d804c924a3d999da",
    ),
    (
        "cr2-canon-5d3.cr2",
        "https://raw.pixls.us/getfile.php/771/nice/Canon%20-%20EOS%205D%20Mark%20III.CR2",
        "ec069b178b9383d80c72f8cdc1b79bd54ca451e7c20793b109d8ad73bef6bdf6",
    ),
    (
        "cr2-canon-80d.cr2",
        "https://raw.pixls.us/getfile.php/1294/nice/Canon%20-%20EOS%2080D%20-%20RAW%20%283:2%29.CR2",
        "8f28465cee09844ffad95dbd3ec91923717a329c5ec4f3f405f5b4fce83aaea3",
    ),
    (
        "cr3-canon-m50-craw.cr3",
        "https://raw.pixls.us/getfile.php/2663/nice/Canon%20-%20EOS%20M50%20-%20CRAW%20%283:2%29.CR3",
        "15384b775867ec4c42b11882837f1e368cedc0561832ffab271221e6bb80be4c",
    ),
    (
        "cr3-canon-m50-raw.cr3",
        "https://raw.pixls.us/getfile.php/4657/nice/Canon%20-%20EOS%20M50%20-%203:2.CR3",
        "1ba9ad6b51b315b88820eb0fe5fd7f52bc136ba37b1a5ca76825ebfe25b18058",
    ),
    (
        "cr3-canon-r100-raw.cr3",
        "https://raw.pixls.us/getfile.php/7896/nice/Canon%20-%20EOS%20R100%20-%20RAW%20%283:2%29.CR3",
        "6d83217d58a5e6d2dabcf470430e91a16676b60531c025b81fa7453fc74b0521",
    ),
    (
        "cr3-canon-r100-craw.cr3",
        "https://raw.pixls.us/getfile.php/7897/nice/Canon%20-%20EOS%20R100%20-%20CRAW%20%283:2%29.CR3",
        "0b66842b2fe00329ebc05fe8ab9357ddbbe1a0a2d2e69a2893102de7fdf5c942",
    ),
    (
        "cr3-canon-r8-raw.cr3",
        "https://raw.pixls.us/getfile.php/6585/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3",
        "7d5c6dbb11ff7e6ee58715d90a20f5d801e8b103f4711ff6ea329ceafcef254c",
    ),
    (
        "cr3-canon-r8-craw.cr3",
        "https://raw.pixls.us/getfile.php/6587/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3",
        "df33cf394573645ce03dea1b2e9f0b5cc2b7734e9e3391ee7114151414cf2812",
    ),
    // EOS R5 Mark II, full frame and 1.6x crop mode: odd-height (2825- / 1787-row) Bayer planes.
    (
        "cr3-canon-r5m2-raw.cr3",
        "https://raw.pixls.us/getfile.php/7881/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20RAW%20%283:2%29.CR3",
        "d6e6aa3b493ea8936dff0efb0cc17c87471437e7317bce37c8c82ba08b9cab7b",
    ),
    (
        "cr3-canon-r5m2-craw.cr3",
        "https://raw.pixls.us/getfile.php/7882/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20CRAW%20%283:2%29.CR3",
        "b3c08a5c7a97299dfd58896a12b939e7b43ec78682761812524825e6e5d087be",
    ),
    (
        "cr3-canon-r5m2-apsc-raw.cr3",
        "https://raw.pixls.us/getfile.php/7883/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20RAW%20%283:2%29.CR3",
        "9f88c0ff012924193b7f4497559d133dfe626766d600b5d67689156721739ead",
    ),
    (
        "cr3-canon-r5m2-apsc-craw.cr3",
        "https://raw.pixls.us/getfile.php/7884/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20CRAW%20%283:2%29.CR3",
        "6a597497776a6abbfab8fcf12d0086ac7a3f8afea2879d61accf6bbf48d357d7",
    ),
    (
        "dng-adobe-canon-5d3-linear-lj92.dng",
        "https://raw.pixls.us/getfile.php/1032/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossless%20JPEG%20compression%2C%20rgb%20%283:2%29.DNG",
        "0608c9f277307b745530e61b589c465540611df22f9a1fe2d01237c5b2b4c13b",
    ),
    (
        "dng-adobe-canon-5d3-lj92.dng",
        "https://raw.pixls.us/getfile.php/1024/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20Lossless%20JPEG%20compression%20%283:2%29.DNG",
        "3118116735d4f6dd01fd9d6a80ee9aac52a2c2debdc6dcd112bd5e3059044a55",
    ),
    (
        "dng-adobe-canon-5d3-lossy.dng",
        "https://raw.pixls.us/getfile.php/1023/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossy%20JPEG%20compression%20%283:2%29.DNG",
        "159326856c29073e845c3c5a9ecf98c6474f43ca15798a88ad5e2baecd0664b7",
    ),
    // Apple ProRAW (iPhone 12 Pro, iOS 14.3): LinearRaw LJ92 tiles, ProfileToneCurve, ProfileGainTableMap, a sky matte.
    (
        "dng-apple-iphone12pro-proraw.dng",
        "https://raw.pixls.us/data/Apple/iPhone%2012%20Pro/IMG_1361.DNG",
        "e91e77a4533ed7cce551d83330676ea5c47dd5e55fb38adda7819366afdbdfc2",
    ),
    (
        "dng-canon-5d3-14bit-small.dng",
        "https://raw.pixls.us/getfile.php/2204/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2014bit%2014bit%20%282.3471882640587%29.dng",
        "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e",
    ),
    (
        "dng-canon-5d3-16bit-169.dng",
        "https://raw.pixls.us/getfile.php/2649/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%20%2816:9%29.dng",
        "bc29ac8a37c800346749b9a5a1adc2722357f7f0e49df05a7104454881604b92",
    ),
    (
        "dng-canon-5d3-16bit.dng",
        "https://raw.pixls.us/getfile.php/885/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20RAW.dng",
        "6275226bfa2d86ba479130999d045ea0b8da6be4b011130c9eb91cd6a0eb2233",
    ),
    (
        "dng-google-pixel2xl.dng",
        "https://raw.pixls.us/getfile.php/2206/nice/Google%20-%20Pixel%202%20XL%20-%2016bit%20%284:3%29.dng",
        "5541093a369f0487fef92bcd1f5fef1be6e4e3cec2aa4d52ad38d2211f3309d7",
    ),
    (
        "dng-ricoh-gr3.dng",
        "https://raw.pixls.us/getfile.php/3115/nice/Ricoh%20-%20GR%20III%20-%2014bit%20%283:2%29.DNG",
        "05513ee72f7cac4165534c9f64995412084f414a033c80c473c79fd94154292e",
    ),
    (
        "nef-nikon-d5100-lossless.nef",
        "https://raw.pixls.us/getfile.php/1597/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.nef",
        "3ce328830942381648be26b372a54b317fdb27b63c1aa9c2ee1fce076a2e1b4f",
    ),
    (
        "nef-nikon-d5100-uncompressed.nef",
        "https://raw.pixls.us/getfile.php/1598/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20uncompressed%20%283:2%29.nef",
        "2fb2f1b87a3b89de5ba9e56527f21a912ca624f2fa2a594cef108ee8d7180412",
    ),
    (
        "nef-nikon-d7000-lossy12.nef",
        "https://raw.pixls.us/getfile.php/961/nice/Nikon%20-%20D7000%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "986db2bf9ed08cf095bb94cfbaf02f0e89d123edad28b63c05133e7815749486",
    ),
    (
        "nef-nikon-d7500-lossless12.nef",
        "https://raw.pixls.us/getfile.php/1532/nice/Nikon%20-%20D7500%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "1f4e58a961d3d317fa39a7e1c8948b825651e968c8cd06e49e4239f612faccad",
    ),
    (
        "nef-nikon-d7500-lossless14.nef",
        "https://raw.pixls.us/getfile.php/1534/nice/Nikon%20-%20D7500%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "430b4f1be4e53a011861b63294ad19fdc0353ec3ea62104f77d4c193e3dd3fc8",
    ),
    (
        "nef-nikon-zf-he.nef",
        "https://raw.pixls.us/download/data/Nikon/Z%20f/DSC_0043.NEF",
        "98d6ca8e6c98048ca7ffed68ccaeda7b2b9f03807f0d320d97d5678db21748c2",
    ),
    (
        "nef-nikon-zf-lossless.nef",
        "https://raw.pixls.us/download/data/Nikon/Z%20f/DSC_0040.NEF",
        "83c82be0be8865d796096dfbcc8ef2abf5af1bd37db44dfad6715070b0c99d15",
    ),
    (
        "nrw-nikon-b700-uncompressed.nrw",
        "https://raw.pixls.us/getfile.php/1621/nice/Nikon%20-%20COOLPIX%20B700%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
        "a175d5892304e79282add07eacf4bdc498ca3f050af48cf989c080355b4735ec",
    ),
    (
        "orf-olympus-e1.orf",
        "https://raw.pixls.us/getfile.php/1800/nice/Olympus%20-%20E-1%20-%2016bit%20%284:3%29.ORF",
        "042286653fbae5b085bef4a4e626145385ea6e82e817f166b4904abcef64457c",
    ),
    (
        "orf-olympus-e400.orf",
        "https://raw.pixls.us/getfile.php/2151/nice/Olympus%20-%20E-400%20-%2016bit%20%284:3%29.ORF",
        "1354e0ac98ea90820227650031d8215637a2aaabd15de995c5a1e00b1986261b",
    ),
    (
        "orf-olympus-em1.orf",
        "https://raw.pixls.us/getfile.php/1051/nice/Olympus%20-%20E-M1%20-%2016bit%20%284:3%29.orf",
        "19ba17e778ae802d4039f0e88bb1cd68825780788b8058c479067e32b9a63eda",
    ),
    (
        "orf-olympus-em10iii.orf",
        "https://raw.pixls.us/getfile.php/1787/nice/Olympus%20-%20E-M10%20Mark%20III%20-%2016bit%20%284:3%29.ORF",
        "11d90cbb564ad2c660c7a8d5d81d99c2c018aa27fdec45ed6c461fa5d24f942b",
    ),
    (
        "orf-olympus-xz2.orf",
        "https://raw.pixls.us/getfile.php/1432/nice/Olympus%20-%20XZ-2%20-%2012bit%20%284:3%29.orf",
        "675fefe7c9e99281783b987889d723b5f46692f5c20e19fed5e422c7c6d2636b",
    ),
    (
        "pef-pentax-k10d.pef",
        "https://raw.pixls.us/getfile.php/2239/nice/Pentax%20-%20K10D%20-%2012bit%2012bit%20compressed%20%283:2%29.PEF",
        "e35ae4154a468be3154f5f462e884ba5941f010d3e8f23d347fbec14809f44d3",
    ),
    (
        "pef-pentax-k3.pef",
        "https://raw.pixls.us/getfile.php/1075/nice/Pentax%20-%20K-3%20-%2014bit%20%283:2%29.PEF",
        "abfb3907b53734b5f353e8de0c6173470c7aad07f9cecc10f723d222a49dec9a",
    ),
    (
        "pef-pentax-k5iis.pef",
        "https://raw.pixls.us/getfile.php/1198/nice/Pentax%20-%20K-5%20II%20s%20-%2014bit%20%283:2%29.PEF",
        "e579e7360c35e512f9c3a5c0eb9519575b87b489c6f86be82590f7706a7ed17b",
    ),
    // Samsung SRW, one file per packing: 12-bit MSB-first (NX10), 12-bit LSB-first (NX20), 16-bit words (EX1).
    (
        "srw-samsung-nx10.srw",
        "https://raw.pixls.us/getfile.php/5913/nice/Samsung%20-%20NX10%20-%2012bit%20%283:2%29.SRW",
        "dead7b4ad83b739ff7b8a1218882589f66cb14efc09f79eeba7a1dcbf7abd29f",
    ),
    (
        "srw-samsung-nx20.srw",
        "https://raw.pixls.us/getfile.php/5370/nice/Samsung%20-%20NX20%20-%2012bit%20%283:2%29.SRW",
        "8a367db506b8f7a6661a6f33a730467cfb3bfd7e46020a9f0df702330a7b45bb",
    ),
    (
        "srw-samsung-ex1.srw",
        "https://raw.pixls.us/getfile.php/1204/nice/Samsung%20-%20EX1%20-%2014bit%20%284:3%29.SRW",
        "a084890fd9d7995ceb746c18ecf5be48c95baf14286d5c0c480cc3daa25f3e37",
    ),
    (
        "raf-fuji-xa2-12bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2883/nice/Fujifilm%20-%20X-A2%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "58ad687b23138c3c49ebeb6c1f80daf13185349be364b947e2bf27e39a462d2f",
    ),
    (
        "raf-fuji-xa5-14bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2526/nice/Fujifilm%20-%20X-A5%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "46daaf8c2b07f40bd01f737181d048a497bb0a87615a1fde65809fdd5de86bf1",
    ),
    (
        "raf-fuji-xe1-12bit.raf",
        "https://raw.pixls.us/getfile.php/3098/nice/Fujifilm%20-%20X-E1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "0fa01fa1a674bcfa40f659225b4e66c5bedf15d2f78407ca1966dc8f38cd0e9f",
    ),
    (
        "raf-fuji-xt20-14bit.raf",
        "https://raw.pixls.us/getfile.php/1177/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "afe552f4a2794c7aa2376dd826165354b243dcf6f2296b01505dded8c8e4be76",
    ),
    (
        "raf-fuji-xt20-compressed.raf",
        "https://raw.pixls.us/getfile.php/1178/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "a23045101b2912e64f154100d646c228c0d16585d21e95e0d1a006e537f7784c",
    ),
    (
        "raw-panasonic-fz50.raw",
        "https://raw.pixls.us/getfile.php/2234/nice/Panasonic%20-%20DMC-FZ50%20-%204:3.RAW",
        "a776a941758214bf4d1342e2b89ed756bfcad5877e038cf9df3bdda502c432ea",
    ),
    (
        "raw-panasonic-fz8.raw",
        "https://raw.pixls.us/getfile.php/2282/nice/Panasonic%20-%20DMC-FZ8%20-%204:3.RAW",
        "a11f1534f3092a4d5f2b57d182f6b06f8d26cf766eb463c3e02b26a338b63b8b",
    ),
    (
        "rw2-panasonic-fz1000m2-4x3.rw2",
        "https://raw.pixls.us/getfile.php/4706/nice/Panasonic%20-%20DC-FZ10002%20-%204:3.RW2",
        "ab30922774bd829a992471d89eeefe1e829565a886d1477958130a2fa4fb77a1",
    ),
    // Fujifilm predictive compression: older X-Trans, 40 MP X-Trans, 14/16-bit GFX; both compression modes.
    (
        "raf-fuji-gfx100-3773.raf",
        "https://raw.pixls.us/getfile.php/3773/nice/Fujifilm%20-%20GFX%20100%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
        "018781e126842596fcf3876b3fb6cd8973dc3936ccc753ee8648618be8889287",
    ),
    (
        "raf-fuji-gfx100-3775.raf",
        "https://raw.pixls.us/getfile.php/3775/nice/Fujifilm%20-%20GFX%20100%20-%2014bit%2014bit%20compressed%20%284:3%29.RAF",
        "0e7dad9819a748bb3d3be26bbf5770d4968e2d80b9cb41d97036bf7c27f7bfe7",
    ),
    (
        "raf-fuji-gfx100rf-8091.raf",
        "https://raw.pixls.us/getfile.php/8091/nice/Fujifilm%20-%20GFX100RF%20-%2016bit%20compressed%20%284:3%29.RAF",
        "14bed5ac8191d6f2a7c82984e3bf6470ad4740df329293eac42b8018cc6a668a",
    ),
    (
        "raf-fuji-gfx100s-4495.raf",
        "https://raw.pixls.us/getfile.php/4495/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20lossless%20compressed%20%284:3%29.RAF",
        "a7bf8f061b603f7ef5fd29276eb6b2ac57355ca2f589a340117bfff0d03c065c",
    ),
    (
        "raf-fuji-gfx100s-4503.raf",
        "https://raw.pixls.us/getfile.php/4503/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
        "1bb4608b61861a3d9e85b0ca6c00689b8ba4620cbe8579374d958c21ed08edf0",
    ),
    (
        "raf-fuji-gfx50s-1435.raf",
        "https://raw.pixls.us/getfile.php/1435/nice/Fujifilm%20-%20GFX%2050S%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "a7124d13910e0241a6b035154bc1fcdb7be4439371d043b1e1950a0251feb51e",
    ),
    (
        "raf-fuji-xe5-8509.raf",
        "https://raw.pixls.us/getfile.php/8509/nice/Fujifilm%20-%20X-E5%20-%2014bit%20lossy%20compressed%20%283:2%29.RAF",
        "28f56cc9784164209e92fae209bdcc78267bd692aa2fb964595485f7e93692ab",
    ),
    (
        "raf-fuji-xh2-6001.raf",
        "https://raw.pixls.us/getfile.php/6001/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "caeadb2666f6c5d9bbe32fbe692c2079b6a7e028f8514a13173f360656d08507",
    ),
    (
        "raf-fuji-xh2-6002.raf",
        "https://raw.pixls.us/getfile.php/6002/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "109be38f59963c9b8ff289ebe1f5ec9168c52f609e5b601432bd194520b4605b",
    ),
    (
        "raf-fuji-xm5-7748.raf",
        "https://raw.pixls.us/getfile.php/7748/nice/Fujifilm%20-%20X-M5%20-%2014bit%20compressed%20%283:2%29.RAF",
        "c76ce546e15727abb4ba201471010c554403176963a1a6b8cc017462606429e5",
    ),
    (
        "raf-fuji-xt2-865.raf",
        "https://raw.pixls.us/getfile.php/865/nice/Fujifilm%20-%20X-T2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "f756ad5be551a8b3e1b503a8a6f7e8e637fba519c0b0f8c79dd58e94b0287602",
    ),
    (
        "raf-fuji-xt4-3914.raf",
        "https://raw.pixls.us/getfile.php/3914/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20lossless%20compressed%20%283:2%29.RAF",
        "07ea642656074834ffe5c661870a612ffa2e80fc1218a05166ee966bbe5f6fae",
    ),
    (
        "raf-fuji-xt4-3918.raf",
        "https://raw.pixls.us/getfile.php/3918/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "871836938ffa695f4c41765ddb107c2703ce755f7f3ced7e10f9e04746f1734e",
    ),
    (
        "raf-fuji-xt5-6122.raf",
        "https://raw.pixls.us/getfile.php/6122/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "b2a067e7fda16338f15c2532caa12b238b2415dc1c75a96a6d24d3445cfd0ae3",
    ),
    (
        "raf-fuji-xt5-6123.raf",
        "https://raw.pixls.us/getfile.php/6123/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "cddd7ae0c43f9280876e5fdcbdf8878718972ebd3d034d8affc8cfc6752d33dc",
    ),
    (
        "raf-fuji-xt50-7807.raf",
        "https://raw.pixls.us/getfile.php/7807/nice/Fujifilm%20-%20X-T50%20-%2014bit%20compressed%20%283:2%29.RAF",
        "3a8a9ac6d92274969aaa1f55ea669191b98bb8f588d00ec8ec17afd8b3963dea",
    ),
    // Sony ILCE-7RM6 ARW6, CC0 1.0 (RawDB): one full-frame Compressed (RAW-C) and one APS-C Compressed HQ (RAW-C-HD) file;
    // docs/arw6-compression.md lists the rest of the set.
    (
        "arw6-sony-ilce7rm6-apsc-land-hq.arw",
        "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_land_RAW-C-HD.ARW",
        "b60a7e752d2a8102ccbe6eebcfdbf533c6b0f2545739090873e9e8eb32cc0637",
    ),
    (
        "arw6-sony-ilce7rm6-ff-land-c.arw",
        "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_land_RAW-C.ARW",
        "d900d6cbe838be864269ad54de4a33fcac7eb3a3d4f91009739f770b8f85a23b",
    ),
    (
        "rw2-panasonic-g9-b.rw2",
        "https://raw.pixls.us/getfile.php/2348/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2",
        "16282357ce0143ff7d36737f82074662db7aa0790ecd309196f406baf0f586a7",
    ),
    (
        "rw2-panasonic-g9.rw2",
        "https://raw.pixls.us/getfile.php/2585/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2",
        "7a56babeb19f9bde3eaa532a4c8d1b11637a0f550963a1182ec845722c65583d",
    ),
    (
        "rw2-panasonic-gh1.rw2",
        "https://raw.pixls.us/getfile.php/1323/nice/Panasonic%20-%20DMC-GH1%20-%204:3.RW2",
        "2b846f932c665f196af9af97b18469730331b60b2c62811969be7db245ce6cce",
    ),
    (
        "rw2-panasonic-gh5.rw2",
        "https://raw.pixls.us/getfile.php/1517/nice/Panasonic%20-%20DC-GH5%20-%204:3.RW2",
        "dae2281df393d8c3c1b8c513d00985f1834ae0e69ce18fbd571370c25d95d22b",
    ),
    (
        "rw2-panasonic-gh5m2.rw2",
        "https://raw.pixls.us/getfile.php/5082/nice/Panasonic%20-%20DC-GH5M2%20-%204:3.RW2",
        "0ea0efd2a9442d8c68508e4196e29f1da4577407dc3df5add4cf360f70493c7b",
    ),
    (
        "rw2-panasonic-gh5s.rw2",
        "https://raw.pixls.us/getfile.php/2603/nice/Panasonic%20-%20DC-GH5S%20-%204:3.RW2",
        "b617151876e62b492641cef14b8c98ee3797e5014d24c6af1c780715a501fa53",
    ),
    (
        "rw2-panasonic-gh6.rw2",
        "https://raw.pixls.us/getfile.php/5876/nice/Panasonic%20-%20DC-GH6%20-%204:3.RW2",
        "07996bca904a178d02c5f56ee03a8dce775a0e1150c1beeaec51d72ff5e700d0",
    ),
    (
        "rw2-panasonic-gx80.rw2",
        "https://raw.pixls.us/getfile.php/1569/nice/Panasonic%20-%20DMC-GX80%20-%204:3.RW2",
        "6e9a419c70c2124630912b2934027c635d3871e3d22dd91fed0655d58cfb5382",
    ),
    (
        "rw2-panasonic-s1.rw2",
        "https://raw.pixls.us/getfile.php/3038/nice/Panasonic%20-%20DC-S1%20-%203:2.RW2",
        "45309a096d6010281e2fa310e6ec2fe1bcc02947fa37b1a2414cfc7377ce4ac6",
    ),
    (
        "rw2-panasonic-s5-format7.rw2",
        "https://raw.pixls.us/getfile.php/6339/nice/Panasonic%20-%20DC-S5%20-%203:2.RW2",
        "3615c85207097180b231f0c89d018eb3f1a71988780612921eb0a05246bca706",
    ),
    (
        "rw2-panasonic-s5m2.rw2",
        "https://raw.pixls.us/getfile.php/7790/nice/Panasonic%20-%20DC-S5M2%20-%2014bit%20%283:2%29.RW2",
        "53a171521a7f9af6910b4ab61c5a4a42de0cceb24166e15b07eee1e00675028f",
    ),
    (
        "rw2-panasonic-s9.rw2",
        "https://raw.pixls.us/getfile.php/7702/nice/Panasonic%20-%20DC-S9%20-%203:2.RW2",
        "290ad5b4e396cb241a59459b2613d2b0659a03ff654e5d5368b3376d35812e50",
    ),
    (
        "rwl-leica-dlux7.rwl",
        "https://raw.pixls.us/getfile.php/4204/nice/Leica%20-%20D-Lux%207%20-%204:3.RWL",
        "4f6db6492da83f7f2a5012591e370557249d1ed44d4c948784016605b799327c",
    ),
    (
        "cr2-canon-eos-70d.cr2",
        "https://raw.pixls.us/getfile.php/784/nice/Canon%20-%20EOS%2070D%20-%20RAW.CR2",
        "00e42517013bd043b7145001399bbc320a8ff235a6dcd16033580d8a130bee1e",
    ),
    (
        "cr2-canon-eos-rebel-t7i.cr2",
        "https://raw.pixls.us/getfile.php/3437/nice/Canon%20-%20EOS%20Rebel%20T7i%20-%20RAW%20%283:2%29.cr2",
        "03fb55af71f384ada6b6aa0ca93279d48cf5fbd13651a323408d2fd97e1d1f7c",
    ),
    (
        "cr2-canon-powershot-g9-x.cr2",
        "https://raw.pixls.us/getfile.php/2154/nice/Canon%20-%20PowerShot%20G9%20X%20-%20RAW%20%283:2%29.CR2",
        "05c041127f4fdecfd49a45da4b12192c8dbe695c3e50427043166fef49b39e83",
    ),
    (
        "pef-pentax-k-1.pef",
        "https://raw.pixls.us/getfile.php/2469/nice/Pentax%20-%20K-1%20-%2014bit%20%283:2%29.PEF",
        "068c902e7b38ac31c7c6eca5c55f69ca395ec6aa7170489281b692a893d0fee9",
    ),
    (
        "cr2-canon-eos-4000d.cr2",
        "https://raw.pixls.us/getfile.php/3241/nice/Canon%20-%20EOS%204000D%20-%20RAW%20%283:2%29.CR2",
        "06daaf663f42d3f6ceb1a837d3ca4db2cfcb5899888ed13510bfa01e3261a896",
    ),
    (
        "cr2-canon-eos-1000d.cr2",
        "https://raw.pixls.us/getfile.php/1616/nice/Canon%20-%20EOS%201000D%20-%20RAW%20%283:2%29.CR2",
        "08b0843a7968315b68bd3c71a96c952ebf275d5b5eba4b6094ae8b103522584c",
    ),
    (
        "cr2-canon-powershot-g1-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/2186/nice/Canon%20-%20PowerShot%20G1%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "08d861ca7fc2890f0217034970d4c900b09373b421106e4ba080f6904a48b37c",
    ),
    (
        "cr2-canon-eos-500d.cr2",
        "https://raw.pixls.us/getfile.php/911/nice/Canon%20-%20EOS%20500D%20-%20RAW%20%283:2%29.CR2",
        "0b57eb8b0dece6f63d4ea61660279488121c9a0aa250113852fa84c051c6da3e",
    ),
    (
        "pef-pentax-ist-dl.pef",
        "https://raw.pixls.us/getfile.php/1083/nice/Pentax%20-%20%2Aist%20DL%20-%2012bit%20%283:2%29.PEF",
        "0e6cd3c999a6dbf121591b2c58aaa8216e625f549db46af627f7346ccf1cf662",
    ),
    (
        "cr2-canon-eos-400d.cr2",
        "https://raw.pixls.us/getfile.php/1764/nice/Canon%20-%20EOS%20400D%20-%20RAW%20%283:2%29.CR2",
        "0e752ddaf81795885efc6443c78e5fee4d5f8f00c8d979ef20e235424f98d595",
    ),
    (
        "dng-ricoh-gx200.dng",
        "https://raw.pixls.us/getfile.php/2589/nice/Ricoh%20-%20GX200%20-%2012bit%20%284:3%29.DNG",
        "0f328905ee6f5b4823040a666103ced651178a13b8ccd0c4c60662f7265dd87f",
    ),
    (
        "cr2-canon-powershot-s110.cr2",
        "https://raw.pixls.us/getfile.php/909/nice/Canon%20-%20PowerShot%20S110%20-%20RAW%20%284:3%29.CR2",
        "14e440316c1d2b005a7f5d5c042e063b3e15d34242e38c3a0983db8ef85e4288",
    ),
    (
        "cr2-canon-eos-kiss-x80.cr2",
        "https://raw.pixls.us/getfile.php/3099/nice/Canon%20-%20EOS%20Kiss%20X80%20-%20RAW%20%283:2%29.CR2",
        "155878290528ff91f67b4f9ffed5d765e80ede5fb0c36fca40107c0b7f5c1d9d",
    ),
    (
        "dng-pentax-kp.dng",
        "https://raw.pixls.us/getfile.php/1824/nice/Pentax%20-%20KP%20-%2014bit%20%283:2%29.DNG",
        "16b09cb3032003989877659ae03cbb6985e3d7e84f1402863ccae8af10033c1c",
    ),
    (
        "dng-pentax-k-1-mark-ii.dng",
        "https://raw.pixls.us/getfile.php/3345/nice/Pentax%20-%20K-1%20Mark%20II%20-%2014bit%20%283:2%29.DNG",
        "190d5878675f4de07d246271ba411b152f3317b2d2a0e27b9c827fe7343b28ea",
    ),
    (
        "dng-pentax-k-3-mark-iii-monochrome.dng",
        "https://raw.pixls.us/getfile.php/7081/nice/Pentax%20-%20K-3%20Mark%20III%20Monochrome%20-%2014bit%20%283:2%29.DNG",
        "1afc1071451a3a3737c29ae319e1411551b6893d0e6a34db29903318266b0066",
    ),
    (
        "cr2-canon-eos-600d.cr2",
        "https://raw.pixls.us/getfile.php/1586/nice/Canon%20-%20EOS%20600D%20-%20RAW%20%283:2%29.CR2",
        "1b0892df5ba5fa418e945f0519d490aeecaaf7e74860a08b5dc47e3e9976a288",
    ),
    (
        "pef-pentax-k-70.pef",
        "https://raw.pixls.us/getfile.php/1141/nice/Pentax%20-%20K-70%20-%2014bit%20%283:2%29.PEF",
        "1d64c6aa71680642ba8f00b6f6105df3e66a4534e631f422fbf3316273c4408c",
    ),
    (
        "dng-ricoh-gr-iiix.dng",
        "https://raw.pixls.us/getfile.php/5818/nice/Ricoh%20-%20GR%20IIIx%20-%2014bit%20%283:2%29.DNG",
        "1e318af31be4baa9949f0ed4af5cfe8ffb2c180e2c12926cd75ac1d029bb0fae",
    ),
    (
        "cr2-canon-powershot-g1-x.cr2",
        "https://raw.pixls.us/getfile.php/1588/nice/Canon%20-%20PowerShot%20G1%20X%20-%20RAW%20%284:3%29.CR2",
        "1e486bbbdfb2c94accf0c5ef4558d1aa484e0ffd36fa03440e3ae34ba2aa338b",
    ),
    (
        "dng-ricoh-gxr.dng",
        "https://raw.pixls.us/getfile.php/2756/nice/Ricoh%20-%20GXR%20-%2012bit%20%284:3%29.DNG",
        "219a062f3d1eb8d66c9dd6088eecf495be082cf5c81f88622eb0dd28cc893cec",
    ),
    (
        "cr2-canon-powershot-g16.cr2",
        "https://raw.pixls.us/getfile.php/1114/nice/Canon%20-%20PowerShot%20G16%20-%20RAW%20%284:3%29.CR2",
        "2307e3cf643ab2f94d03f899467d8797ce1ef41bbffd6ad50af81bdd0ba34098",
    ),
    (
        "dng-pentax-k20d.dng",
        "https://raw.pixls.us/getfile.php/1143/nice/Pentax%20-%20K20D%20-%2012bit%20%283:2%29.DNG",
        "264465ef1d97f1599899778589f07f8aa485b88c0dfc9b1d4c006bf79d44b420",
    ),
    (
        "cr2-canon-powershot-g3-x.cr2",
        "https://raw.pixls.us/getfile.php/1509/nice/Canon%20-%20PowerShot%20G3%20X%20-%20RAW%20%283:2%29.CR2",
        "267641913613fd52532279ff16f03c135511fbcab5bbe8b276fc9cf91aebd7ca",
    ),
    (
        "pef-pentax-ist-ds.pef",
        "https://raw.pixls.us/getfile.php/4392/nice/Pentax%20-%20%2Aist%20DS%20-%2012bit%2012bit%20uncompressed%20%283:2%29.pef",
        "27809a7d252d515685ed4e6091b70ab25348502baeb75db03ce7b4f1d9549b76",
    ),
    (
        "cr2-canon-eos-m.cr2",
        "https://raw.pixls.us/getfile.php/1224/nice/Canon%20-%20EOS%20M%20-%20RAW%20%283:2%29.CR2",
        "27d151efe86f04bea8fcf6ec9f3a722ae3ee24a3f56a6ebbd27ee89b215cf569",
    ),
    (
        "cr2-canon-eos-1d-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1058/nice/Canon%20-%20EOS-1D%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "2e372710d546c9b8903a343696af4e7fd245794571f64258ab607a89dfa6aa97",
    ),
    (
        "pef-pentax-k-3-mark-iii.pef",
        "https://raw.pixls.us/getfile.php/4677/nice/Pentax%20-%20K-3%20Mark%20III%20-%2014bit%20%283:2%29.PEF",
        "351be88038b2dd28af39e11dbece6de74efd71119a9ef667d35cdf02dcff919f",
    ),
    (
        "cr2-canon-eos-750d.cr2",
        "https://raw.pixls.us/getfile.php/1982/nice/Canon%20-%20EOS%20750D%20-%20RAW%20%283:2%29.CR2",
        "38cdb173fe1ea083f4d475285d3080db49d7f3c7cdcb49bf1968fceeed66c85c",
    ),
    (
        "cr2-canon-eos-5ds.cr2",
        "https://raw.pixls.us/getfile.php/2083/nice/Canon%20-%20EOS%205DS%20-%20RAW%20%283:2%29.CR2",
        "3fdb8b7ada3b4a69ed5756fa814bee9bd51c1c22b40e70252a46890e2855df07",
    ),
    (
        "dng-canon-powershot-s2-is.dng",
        "https://raw.pixls.us/getfile.php/941/nice/Canon%20-%20PowerShot%20S2%20IS%20-%2010bit%2010bit%20DNG%2C%20CHDK%20ver.%201.0.0-1504%20%284:3%29.CR2",
        "445764716df165a1facd37c01d67ede44ca49671ed1a6112dd8ff7426324c757",
    ),
    (
        "cr2-canon-powershot-s100v.cr2",
        "https://raw.pixls.us/getfile.php/2492/nice/Canon%20-%20PowerShot%20S100V%20-%20RAW%20%284:3%29.CR2",
        "4775f347686f785104ee7f566df158247cdecef4de76fdbfa150e18b6b104c0a",
    ),
    (
        "cr2-canon-eos-kiss-digital-n.cr2",
        "https://raw.pixls.us/getfile.php/1185/nice/Canon%20-%20EOS%20Kiss%20Digital%20N%20-%20RAW%20%283:2%29.CR2",
        "49c78e5c36255cdd3f9fd219e86e3325bfa67be4d8d1ab5e21c21c3fd53c294e",
    ),
    (
        "dng-pentax-k-01.dng",
        "https://raw.pixls.us/getfile.php/1008/nice/Pentax%20-%20K-01%20-%2012bit%20%283:2%29.DNG",
        "4c9aac2ce2720154616ad7678385d1ec1f626d6464a1d5c6a5a4d821a78a608d",
    ),
    (
        "cr2-canon-eos-rebel-t5.cr2",
        "https://raw.pixls.us/getfile.php/1411/nice/Canon%20-%20EOS%20Rebel%20T5%20-%20RAW%20%283:2%29.CR2",
        "4f71c1416037d2212cc4847e978bffe874afc0f653260386e96dff704ca8bf51",
    ),
    (
        "pef-pentax-k110d.pef",
        "https://raw.pixls.us/getfile.php/1309/nice/Pentax%20-%20K110D%20-%2012bit%20%283:2%29.pef",
        "4f7c5574b0dbc3c1270b269126c2a3be030a86799f8b544610c74f31ef782f72",
    ),
    (
        "cr2-canon-powershot-g11.cr2",
        "https://raw.pixls.us/getfile.php/2755/nice/Canon%20-%20PowerShot%20G11%20-%20RAW%20%284:3%29.CR2",
        "50ef4d6712c7add1c65ef4c48c74ac5017f24ff1789f1d8a8d5d9accd3aab0fa",
    ),
    (
        "cr2-canon-eos-rebel-sl1.cr2",
        "https://raw.pixls.us/getfile.php/1201/nice/Canon%20-%20EOS%20Rebel%20SL1%20-%20RAW%20%283:2%29.CR2",
        "56faa7db3947c32c5e6e31bc0db027ebf95163533e043bd8c4f18166a129692c",
    ),
    (
        "cr2-canon-powershot-g15.cr2",
        "https://raw.pixls.us/getfile.php/1986/nice/Canon%20-%20PowerShot%20G15%20-%20RAW%20%284:3%29.CR2",
        "5756d1a6522134ee4833a9cbef04eb54b26a1f7935e58063733a264478385a26",
    ),
    (
        "dng-pentax-q7.dng",
        "https://raw.pixls.us/getfile.php/1760/nice/Pentax%20-%20Q7%20-%2012bit%20%284:3%29.DNG",
        "577eac8d03419bb82714169dd532d58efc82a0abf928d28e4641f1ed5d56c350",
    ),
    (
        "dng-pentax-k-s1.dng",
        "https://raw.pixls.us/getfile.php/1365/nice/Pentax%20-%20K-S1%20-%2012bit%20%283:2%29.DNG",
        "58abc43a8e025dcab0053bcee173d20197aa2b7983e5e62097147a1e8545f111",
    ),
    (
        "cr2-canon-eos-1d-mark-iv.cr2",
        "https://raw.pixls.us/getfile.php/4258/nice/Canon%20-%20EOS-1D%20Mark%20IV%20-%20RAW%20%283:2%29.CR2",
        "597bb01f2400b04bfe7ee024016992e8e897b7dffaa5e368b8375daf3b32b3ed",
    ),
    (
        "cr2-canon-eos-m5.cr2",
        "https://raw.pixls.us/getfile.php/2935/nice/Canon%20-%20EOS%20M5%20-%20RAW%20%283:2%29.CR2",
        "5af02dbad6cd90eeac2b0b1c0be0aeb9a5d2841c6dd984ee415ede9f2566667d",
    ),
    (
        "cr2-canon-powershot-s100.cr2",
        "https://raw.pixls.us/getfile.php/1357/nice/Canon%20-%20PowerShot%20S100%20-%20RAW%20%284:3%29.CR2",
        "5d1bb0597da59bc920442c55dbd7f70d090a9aa01b5642607bcdde523ae892c6",
    ),
    (
        "pef-pentax-ist-d.pef",
        "https://raw.pixls.us/getfile.php/1843/nice/Pentax%20-%20%2Aist%20D%20-%2012bit%20%283:2%29.PEF",
        "61904b594d16983070f3e56d663ae8f7b246b8bb554c4cf7bd3eb4b9f9a9f3aa",
    ),
    (
        "dng-pentax-k-30.dng",
        "https://raw.pixls.us/getfile.php/3094/nice/Pentax%20-%20K-30%20-%2012bit%20%283:2%29.DNG",
        "63eaa1ff93ad7c89947017332b10f78f17cd3853a5b495adf25410edaefbe618",
    ),
    (
        "cr2-canon-eos-rebel-t4i.cr2",
        "https://raw.pixls.us/getfile.php/2203/nice/Canon%20-%20EOS%20Rebel%20T4i%20-%20RAW%20%283:2%29.CR2",
        "65257f38bc68a11b9185ac710b3a442a19aa1c082afb01b668bf33e6836b6b2d",
    ),
    (
        "cr2-canon-powershot-sx50-hs.cr2",
        "https://raw.pixls.us/getfile.php/3152/nice/Canon%20-%20PowerShot%20SX50%20HS%20-%20RAW%20%284:3%29.CR2",
        "6c2ddd232f5432fb7bd0e28efc12d66995cedcd847f801fd854bfdfb180db50c",
    ),
    (
        "dng-pentax-k200d.dng",
        "https://raw.pixls.us/getfile.php/5246/nice/Pentax%20-%20K200D%20-%2012bit%2012bit%20uncompressed%20%283:2%29.DNG",
        "6d13579dd0a8ec784bfa3d8b53af5a3f936171bfa62d74eb96a17236a6fb9472",
    ),
    (
        "cr2-canon-eos-m2.cr2",
        "https://raw.pixls.us/getfile.php/5067/nice/Canon%20-%20EOS%20M2%20-%20RAW%20%283:2%29.cr2",
        "6d26eeb1e8a5978c7cb9d8beb6e6e34f3c27847055b76bc7f186503ca44508d0",
    ),
    (
        "cr2-canon-eos-m3.cr2",
        "https://raw.pixls.us/getfile.php/1002/nice/Canon%20-%20EOS%20M3%20-%20RAW%20%283:2%29.CR2",
        "6f12a02a21f64b284328ee6d3b2d1bab4439720cad2e881adf3da72592128b11",
    ),
    (
        "cr2-canon-powershot-g12.cr2",
        "https://raw.pixls.us/getfile.php/1767/nice/Canon%20-%20PowerShot%20G12%20-%20RAW%20%284:3%29.cr2",
        "6f2750c7cb144eee80a91431e2ce8df66dd229c3e1f1453bf80409ca669a8614",
    ),
    (
        "dng-pentax-k-5.dng",
        "https://raw.pixls.us/getfile.php/1479/nice/Pentax%20-%20K-5%20-%2014bit%20%283:2%29.DNG",
        "6f6419209340d07d597ccca66dc367d64cff3308ef2694c8f90c6259f3a5c431",
    ),
    (
        "cr2-canon-eos-6d-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1625/nice/Canon%20-%20EOS%206D%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "712815b910fa473f7aaeb97c0804292d996b16dd6b974a3ab9a7195a2e41d90c",
    ),
    (
        "cr2-canon-eos-m10.cr2",
        "https://raw.pixls.us/getfile.php/1758/nice/Canon%20-%20EOS%20M10%20-%20RAW%20%283:2%29.CR2",
        "71e27b9ebe7efba632e666db77b82c5129e0d1a402ff359a2b509774ebeb1b0f",
    ),
    (
        "dng-pentax-k-x.dng",
        "https://raw.pixls.us/getfile.php/830/nice/Pentax%20-%20K-x%20-%2012bit%20%283:2%29.DNG",
        "7a594b66b54f99bb5aba4627c78abfc41213d6911ea54e4f5d2dd6c8505f6968",
    ),
    (
        "cr2-canon-eos-rebel-t5i.cr2",
        "https://raw.pixls.us/getfile.php/2651/nice/Canon%20-%20EOS%20Rebel%20T5i%20-%20RAW%20%283:2%29.CR2",
        "7c3c9c3b8ec487d56040c21620aafe51e13e2c2fc1e1351929970720e10dda69",
    ),
    (
        "dng-ricoh-gr.dng",
        "https://raw.pixls.us/getfile.php/996/nice/Ricoh%20-%20GR%20-%2012bit%20%284:3%29.DNG",
        "7ceec1ea31c56b6e0414202504581b915f17818ffe018b4c8bba1393332c405e",
    ),
    (
        "dng-pentax-645d.dng",
        "https://raw.pixls.us/getfile.php/2401/nice/Pentax%20-%20645D%20-%2014bit%2014bit%20compressed%20%284:3%29.DNG",
        "7cfa4a6b3fc459157b0be94effaf98136027b1182dac7da0f91e29987ab6ae6c",
    ),
    (
        "cr2-canon-eos-rebel-t3.cr2",
        "https://raw.pixls.us/getfile.php/3920/nice/Canon%20-%20EOS%20Rebel%20T3%20-%20RAW%20%283:2%29.CR2",
        "8106f288e22ca0510b2ebe36e58833afbf847db92e9e1ec6f651605bac0beb24",
    ),
    (
        "cr2-canon-eos-77d.cr2",
        "https://raw.pixls.us/getfile.php/1332/nice/Canon%20-%20EOS%2077D%20-%20RAW%20%283:2%29.CR2",
        "811df582d97918987312fa0fceae3ad19a20532a64ebda9782764ac7e629c265",
    ),
    (
        "cr2-canon-eos-2000d.cr2",
        "https://raw.pixls.us/getfile.php/2612/nice/Canon%20-%20EOS%202000D%20-%20RAW%20%283:2%29.CR2",
        "84015493df96c877c0387071a627d6940187d75695b0598694901ac0ff83876a",
    ),
    (
        "cr2-canon-eos-5d-mark-iv.cr2",
        "https://raw.pixls.us/getfile.php/980/nice/Canon%20-%20EOS%205D%20Mark%20IV%20-%20RAW%20%283:2%29.CR2",
        "85bfec149261ffbae90797e45caae5e204e1e61ac62b92783925b5ee0f267d76",
    ),
    (
        "cr2-canon-eos-1d-mark-iii.cr2",
        "https://raw.pixls.us/getfile.php/2037/nice/Canon%20-%20EOS-1D%20Mark%20III%20-%20RAW%20%283:2%29.CR2",
        "860b5c9791548db8081823e3852c436a439e648ea863b0def05e99aac6e9e726",
    ),
    (
        "cr2-canon-eos-60d.cr2",
        "https://raw.pixls.us/getfile.php/993/nice/Canon%20-%20EOS%2060D%20-%20RAW%20%283:2%29.CR2",
        "865236ece3bf1f2f19a15001a8f0f99badf71e34b9eed1fead04d15daf87a154",
    ),
    (
        "pef-pentax-k100d.pef",
        "https://raw.pixls.us/getfile.php/2262/nice/Pentax%20-%20K100D%20-%2012bit%20%283:2%29.PEF",
        "86ec2383dbfda3eda38ca913b02874a70248efba6107aa304781c8d3779de8c9",
    ),
    (
        "cr2-canon-eos-1d-mark-ii-n.cr2",
        "https://raw.pixls.us/getfile.php/1781/nice/Canon%20-%20EOS-1D%20Mark%20II%20N%20-%20RAW%20%283:2%29.CR2",
        "8bf6591f64b0657bc6133cbd0ee1418ef8e6dd3dd595827f8e9910c0f235843c",
    ),
    (
        "dng-pentax-k-500.dng",
        "https://raw.pixls.us/getfile.php/1458/nice/Pentax%20-%20K-500%20-%2012bit%20%283:2%29.DNG",
        "91ccd6f29b8eaad88ae26d1c860ef1113e6dfc1bd2c8f5245b49f3a788295fb1",
    ),
    (
        "cr2-canon-eos-m100.cr2",
        "https://raw.pixls.us/getfile.php/3531/nice/Canon%20-%20EOS%20M100%20-%20RAW%20%283:2%29.CR2",
        "941d9e696b1d92b4b25a762474e3c5c127e7405dea099c266fa85e39a2454fcc",
    ),
    (
        "pef-pentax-k2000.pef",
        "https://raw.pixls.us/getfile.php/5239/nice/Pentax%20-%20K2000%20-%2012bit%2012bit%20compressed%20%283:2%29.PEF",
        "985003748c094b8d02f2517b4c07d66d48ea5380260235ccb3409f41ceb3ea84",
    ),
    (
        "dng-pentax-k-50.dng",
        "https://raw.pixls.us/getfile.php/2235/nice/Pentax%20-%20K-50%20-%2012bit%20%283:2%29.DNG",
        "9ea1dc1f92df19699aa8b620670c64a84d4ead0172a49cbbd47dea9257d14e67",
    ),
    (
        "cr2-canon-powershot-g5-x.cr2",
        "https://raw.pixls.us/getfile.php/4257/nice/Canon%20-%20PowerShot%20G5%20X%20-%20RAW%20%283:2%29.CR2",
        "a7d0ac7f3c7ddfa607a6d0a7433ef9ba664d260aa91616cd75cfbdf912b0da00",
    ),
    (
        "dng-pentax-645z.dng",
        "https://raw.pixls.us/getfile.php/2403/nice/Pentax%20-%20645Z%20-%2014bit%2014bit%20compressed%20%284:3%29.DNG",
        "a8203686c61962af591b374331b6eb9dace4f2b29fce9f166303fc66fd210bb0",
    ),
    (
        "dng-pentax-k-5-ii.dng",
        "https://raw.pixls.us/getfile.php/873/nice/Pentax%20-%20K-5%20II%20-%2014bit%20%283:2%29.DNG",
        "aa41f0d9ab8d3e21e468b4c61d4d1ddae6a4fbe37f629b78159246cbb94dd56b",
    ),
    (
        "cr2-canon-powershot-g9.cr2",
        "https://raw.pixls.us/getfile.php/760/nice/Canon%20-%20PowerShot%20G9%20-%20RAW%20%284:3%29.CR2",
        "ad27a4d751984cd709380e3be274a4a5243d36ca89c5fc711736ba8a9f5c932f",
    ),
    (
        "dng-pentax-q.dng",
        "https://raw.pixls.us/getfile.php/1610/nice/Pentax%20-%20Q%20-%2012bit%20%284:3%29.DNG",
        "ad79b4ce4b88e5ca8b03d0d5aeaba09f55559695234941a5a6ada0cd4825860d",
    ),
    (
        "dng-pentax-mx-1.dng",
        "https://raw.pixls.us/getfile.php/1731/nice/Pentax%20-%20MX-1%20-%2012bit%20%284:3%29.dng",
        "b00f3fd42479bbaba7a00b2b6fa3e5f5d213c51d591adff4f6eb4a768ecd1ce9",
    ),
    (
        "cr2-canon-eos-5d.cr2",
        "https://raw.pixls.us/getfile.php/1346/nice/Canon%20-%20EOS%205D%20-%20RAW%20%283:2%29.CR2",
        "b30089172973388d03e192a14a63f750e22914dfc01db5713b6b3bce3c9baeee",
    ),
    (
        "pef-pentax-k100d-super.pef",
        "https://raw.pixls.us/getfile.php/5804/nice/Pentax%20-%20K100D%20Super%20-%2012bit%2012bit%20uncompressed%20%283:2%29.PEF",
        "b391c84ff1cc8769ec5214f837d1e4b3ae80374673608601b12ee4484e003ddb",
    ),
    (
        "cr2-canon-eos-m6.cr2",
        "https://raw.pixls.us/getfile.php/1382/nice/Canon%20-%20EOS%20M6%20-%20RAW%20%283:2%29.CR2",
        "b3a7a0037600b61d4c26ad03d5cac1801e35fb43a55699db5b77708c96f7338a",
    ),
    (
        "cr2-canon-powershot-g7-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1682/nice/Canon%20-%20PowerShot%20G7%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "b81acebf5ac49b0389c354942d128d9423fdd59ff92217554f232f94e3e87ef5",
    ),
    (
        "cr2-canon-eos-7d-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1064/nice/Canon%20-%20EOS%207D%20Mark%20II%20-%20RAW%20%283:2%29.cr2",
        "b9f71c56adb6e2861e6653efb312fb5cc88ba4aa17fa809c6a18b8db044360c3",
    ),
    (
        "dng-ricoh-gr-ii.dng",
        "https://raw.pixls.us/getfile.php/1955/nice/Ricoh%20-%20GR%20II%20-%2012bit%20%283:2%29.DNG",
        "bc8e00ccfd8e4753e8ee0735255bff4dcca5fb8d1a1352adfa59b86806d03f44",
    ),
    (
        "cr2-canon-powershot-s120.cr2",
        "https://raw.pixls.us/getfile.php/1694/nice/Canon%20-%20PowerShot%20S120%20-%20RAW%20%284:3%29.CR2",
        "bcbfadad8ecdbcc783864a2b769aef37588e2a6339b9682dcd9642602bd2011f",
    ),
    (
        "cr2-canon-powershot-s95.cr2",
        "https://raw.pixls.us/getfile.php/1742/nice/Canon%20-%20PowerShot%20S95%20-%20RAW%20%284:3%29.cr2",
        "c4a55c61434b8403f948518deca22e8ab755099f124d5cea5bc1cd0ea86711ad",
    ),
    (
        "cr2-canon-powershot-g1-x-mark-iii.cr2",
        "https://raw.pixls.us/getfile.php/2695/nice/Canon%20-%20PowerShot%20G1%20X%20Mark%20III%20-%20RAW%20%283:2%29.CR2",
        "ca052ed025432fdacfba7347cb4374fa3b727155912a91c42ed9a275d13bedf5",
    ),
    (
        "cr2-canon-eos-50d.cr2",
        "https://raw.pixls.us/getfile.php/1792/nice/Canon%20-%20EOS%2050D%20-%20RAW%20%283:2%29.CR2",
        "ce460cf2b6762c8c22ebeb00e9e1edccd728edcb7c4e82c78f3bce61d21fa88d",
    ),
    (
        "dng-pentax-k-s2.dng",
        "https://raw.pixls.us/getfile.php/1626/nice/Pentax%20-%20K-S2%20-%2012bit%20%283:2%29.DNG",
        "d23916a31f09c961e6050e4120bbcd92f7847080f9e40fad8dc7047ead1dec2d",
    ),
    (
        "cr2-canon-eos-20d.cr2",
        "https://raw.pixls.us/getfile.php/1018/nice/Canon%20-%20EOS%2020D%20-%20RAW%20%283:2%29.CR2",
        "db78660d48589130d99e0bc621424851eb0f84df052634fb5bc89c7a02fe7033",
    ),
    (
        "pef-pentax-k-r.pef",
        "https://raw.pixls.us/getfile.php/8171/nice/Pentax%20-%20K-r%20-%2012bit%20compressed%20%283:2%29.PEF",
        "dd0d7b76d89039995d253adddcfb44d131414fad2179610d9ea4d8d4bb876b7b",
    ),
    (
        "pef-pentax-kf.pef",
        "https://raw.pixls.us/getfile.php/6664/nice/Pentax%20-%20KF%20-%2014bit%20%283:2%29.PEF",
        "ddc9cdf399a7210b203dee1765ab9f4165a2e695da59bce6bcabf75b77174a4b",
    ),
    (
        "cr2-canon-powershot-g7-x.cr2",
        "https://raw.pixls.us/getfile.php/1277/nice/Canon%20-%20PowerShot%20G7%20X%20-%20RAW%20%283:2%29.CR2",
        "e4b0a64df6f43252ab96b4789f698ca334e86586ab151d1e9f5616b80172abcd",
    ),
    (
        "cr2-canon-eos-450d.cr2",
        "https://raw.pixls.us/getfile.php/1353/nice/Canon%20-%20EOS%20450D%20-%20RAW%20%283:2%29.CR2",
        "e975f71429b939701dd618a7bac4709c1409f4bb6aa1c3607ceeaa86031c1c4a",
    ),
    (
        "cr2-canon-powershot-sx60-hs.cr2",
        "https://raw.pixls.us/getfile.php/4942/nice/Canon%20-%20PowerShot%20SX60%20HS%20-%20RAW%20%284:3%29.CR2",
        "f005e82fe8579876d7e9d39291c57f4410412904dfff0de5bbad9f32b3189612",
    ),
    (
        "dng-pentax-k-3-ii.dng",
        "https://raw.pixls.us/getfile.php/1363/nice/Pentax%20-%20K-3%20II%20-%2014bit%20%283:2%29.DNG",
        "f43b9dc35e4d1b43918d2d4bd0973bc202af48ed24c924e4aff61250c971b171",
    ),
    (
        "dng-pentax-k-7.dng",
        "https://raw.pixls.us/getfile.php/827/nice/Pentax%20-%20K-7%20-%2012bit%20%283:2%29.DNG",
        "f441c36678dfca4f38e25a580f4f8485d78d8067a054153bc36be71e56c92386",
    ),
    (
        "cr2-canon-eos-kiss-x9.cr2",
        "https://raw.pixls.us/getfile.php/2062/nice/Canon%20-%20EOS%20Kiss%20X9%20-%20RAW%20%283:2%29.CR2",
        "f50cea4641c97c1154a214a686226ba900002f00f4f844af858ef80b9ec8dea6",
    ),
    (
        "cr2-canon-powershot-g9-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1575/nice/Canon%20-%20PowerShot%20G9%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "f91f1a3b6fa66b2e15fd26c1bd78e1eb19dfa7e6a3886aff36e7ab96fd1c319c",
    ),
    (
        "cr2-canon-eos-760d.cr2",
        "https://raw.pixls.us/getfile.php/2094/nice/Canon%20-%20EOS%20760D%20-%20RAW%20%283:2%29.cr2",
        "f98108c5efd57eef483009cdb490ada2071d1c1b58a82df3de997b4d86d849c2",
    ),
    (
        "cr2-canon-eos-30d.cr2",
        "https://raw.pixls.us/getfile.php/1791/nice/Canon%20-%20EOS%2030D%20-%20RAW%20%283:2%29.CR2",
        "fb216f8df8d7abca4eb9d9f8dc594c1d2b69d0a6369fb8b4107743343c37144f",
    ),
];

fn cmd_corpus(download: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    println!(
        "Test corpora live under {} (git-ignored, never committed).
Tests that use a corpus skip cleanly when it is absent.

  corpus/raw/        raw.pixls.us samples (CC0) — lightcraft-raw decodes every file.
  corpus/images/     CC0 / public-domain JPEG/PNG/TIFF/HEIC samples (optional)
",
        corpus.display()
    );
    if !download {
        return Ok(());
    }
    let dest = corpus.join("raw");
    std::fs::create_dir_all(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut bad = Vec::new();
    for (name, url, sha256) in RAW_SAMPLES {
        let out = dest.join(name);
        if !out.exists() {
            let mut curl = Command::new("curl");
            curl.args(["-fsSL", "-o"]).arg(&out).arg(url);
            run(curl, &format!("curl {url}"))?;
        }
        let actual = file_sha256(&out)?;
        if actual != *sha256 {
            // delete it, so the next run downloads it again instead of trusting it
            std::fs::remove_file(&out).map_err(|e| format!("remove {}: {e}", out.display()))?;
            bad.push(format!("{name}: sha256 is {actual}, expected {sha256} (file deleted; source {url})"));
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} corpus file(s) do not match their pinned sha256:
  {}",
        bad.len(),
        bad.join(
            "
  "
        )
    ))
}

fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod ci_jobs_tests {
    use super::ci_jobs;

    #[test]
    fn jobs_follow_available_ram_and_never_exceed_the_cpus() {
        assert_eq!(ci_jobs(Some(3 * 1024), 8), 2, "a 4 GB machine with 3 GB free");
        assert_eq!(ci_jobs(Some(6 * 1024), 8), 4, "an 8 GB machine with 6 GB free");
        assert_eq!(ci_jobs(Some(7 * 1024), 32), 4, "a busy 32 GB machine: what is free counts");
        assert_eq!(ci_jobs(Some(24 * 1024), 32), 16);
        assert_eq!(ci_jobs(Some(64 * 1024), 8), 8, "capped at the CPU count");
        assert_eq!(ci_jobs(Some(500), 8), 1, "at least one");
        assert_eq!(ci_jobs(None, 32), 4, "unknown RAM keeps the old default");
        assert_eq!(ci_jobs(None, 2), 2);
    }
}

#[cfg(test)]
mod corpus_tests {
    use super::RAW_SAMPLES;

    #[test]
    fn every_sample_has_a_unique_name_and_a_sha256() {
        let mut names: Vec<_> = RAW_SAMPLES.iter().map(|s| s.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RAW_SAMPLES.len(), "duplicate file name");
        for (name, _, sha256) in RAW_SAMPLES {
            assert!(sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{name}: not a lowercase sha256");
        }
    }
}
