#![expect(clippy::print_stdout, reason = "prints measurements")]

use std::time::Instant;

use rolldown::{Bundler, BundlerOptions, ExperimentalOptions, InputItem, OutputFormat, Platform};
use sugar_path::SugarPath;

// Bundles one entry repeatedly, without and with `experimental.effectServiceShaking`,
// alternating which goes first, and reports the output sizes, every build's time, the host's
// one-minute load average around each pair, and the medians and their ratio.
//
// cargo run --release --example effect_service_shaking -- <entry> <out-dir> [runs]

async fn build(entry: &std::path::Path, out_dir: &std::path::Path, enabled: bool) -> f64 {
  let mut bundler = Bundler::new(BundlerOptions {
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: entry.to_string_lossy().into_owned(),
    }]),
    cwd: Some(entry.parent().unwrap().to_path_buf()),
    dir: Some(out_dir.to_string_lossy().into_owned()),
    format: Some(OutputFormat::Esm),
    platform: Some(Platform::Node),
    experimental: Some(ExperimentalOptions {
      effect_service_shaking: Some(enabled),
      // Module-path comments depend on the installation layout (pnpm's store paths), not
      // on the code: without them, sizes compare across checkouts.
      attach_debug_info: Some(rolldown::AttachDebugInfo::None),
      ..Default::default()
    }),
    ..Default::default()
  })
  .expect("Failed to create bundler");
  let start = Instant::now();
  bundler.write().await.expect("build failed");
  start.elapsed().as_secs_f64() * 1000.0
}

/// The host's one-minute load average (Linux), or `NaN` where it can't be read.
fn load() -> f64 {
  std::fs::read_to_string("/proc/loadavg")
    .ok()
    .and_then(|s| s.split_whitespace().next().and_then(|l| l.parse().ok()))
    .unwrap_or(f64::NAN)
}

fn median(times: &[f64]) -> f64 {
  let mut t = times.to_vec();
  t.sort_unstable_by(f64::total_cmp);
  let n = t.len();
  if n % 2 == 1 { t[n / 2] } else { f64::midpoint(t[n / 2 - 1], t[n / 2]) }
}

#[tokio::main]
async fn main() {
  let args: Vec<String> = std::env::args().collect();
  // A runaway analysis stops at `ROLLDOWN_TEST_MAX_RSS_MB` instead of
  // exhausting the host.
  let _memory_guard = rolldown_testing::memory_guard::guard("effect_service_shaking example");
  let entry = std::path::Path::new(&args[1]).absolutize().into_owned();
  let out = std::path::Path::new(&args[2]).absolutize().into_owned();
  let runs: usize = args.get(3).and_then(|n| n.parse().ok()).unwrap_or(7);
  // Alternate the two configurations, and which of them goes first, so machine load and
  // warm caches affect both alike.
  let mut times = [vec![], vec![]];
  let mut loads = vec![];
  for run in 0..runs {
    let before = load();
    let order = if run % 2 == 0 { [false, true] } else { [true, false] };
    for enabled in order {
      let dir = out.join(if enabled { "on" } else { "off" });
      times[usize::from(enabled)].push(build(&entry, &dir, enabled).await);
    }
    let after = load();
    loads.push(before.max(after));
    println!(
      "run {}: off {:.1} ms, on {:.1} ms, ratio {:.2}, load {before:.1} -> {after:.1}",
      run + 1,
      times[0][run],
      times[1][run],
      times[1][run] / times[0][run],
    );
  }
  for (index, label) in ["off", "on"].into_iter().enumerate() {
    let size =
      std::fs::metadata(out.join(label).join("main.js")).map(|m| m.len()).unwrap_or_default();
    let t = &times[index];
    let min = t.iter().copied().fold(f64::INFINITY, f64::min);
    println!("{label}: {size} bytes, median {:.1} ms, min {min:.1} ms", median(t));
  }
  let ratios: Vec<f64> = times[0].iter().zip(&times[1]).map(|(off, on)| on / off).collect();
  println!(
    "ratio of medians {:.2}, median of per-run ratios {:.2}, median load {:.1}",
    median(&times[1]) / median(&times[0]),
    median(&ratios),
    median(&loads),
  );
}
