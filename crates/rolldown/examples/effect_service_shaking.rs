#![expect(clippy::print_stdout, reason = "prints measurements")]

use std::time::Instant;

use rolldown::{Bundler, BundlerOptions, ExperimentalOptions, InputItem, OutputFormat, Platform};
use sugar_path::SugarPath;

// Bundles one entry twice, without and with `experimental.effectServiceShaking`,
// and reports the output size and the median build time of each.
//
// cargo run --release --example effect_service_shaking -- <entry> <out-dir> [runs]

async fn build(entry: &std::path::Path, out_dir: &std::path::Path, enabled: bool) -> u128 {
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
      ..Default::default()
    }),
    ..Default::default()
  })
  .expect("Failed to create bundler");
  let start = Instant::now();
  bundler.write().await.expect("build failed");
  start.elapsed().as_micros()
}

#[tokio::main]
async fn main() {
  let args: Vec<String> = std::env::args().collect();
  let entry = std::path::Path::new(&args[1]).absolutize().into_owned();
  let out = std::path::Path::new(&args[2]).absolutize().into_owned();
  let runs: usize = args.get(3).and_then(|n| n.parse().ok()).unwrap_or(5);
  // Interleave the two configurations so machine load affects both alike.
  let mut times = [vec![], vec![]];
  for _ in 0..runs {
    for (index, enabled) in [false, true].into_iter().enumerate() {
      let dir = out.join(if enabled { "on" } else { "off" });
      times[index].push(build(&entry, &dir, enabled).await);
    }
  }
  for (index, label) in ["off", "on"].into_iter().enumerate() {
    let times = &mut times[index];
    times.sort_unstable();
    let size =
      std::fs::metadata(out.join(label).join("main.js")).map(|m| m.len()).unwrap_or_default();
    println!("{label}: {size} bytes, median {} ms", times[times.len() / 2] / 1000);
  }
}
