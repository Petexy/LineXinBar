//! Read-only achievement diagnostics through LXB's existing account session.
//! cargo run -p lxb-steam --example probe-achievements -- APP_ID
//! Prints counts only; never credentials, account identifiers or achievement spoilers.
use lxb_steam::{Event, Steam};
use std::time::{Duration, Instant};
fn main() {
    let app_id: u32 = std::env::args()
        .nth(1)
        .expect("provide a Steam app id")
        .parse()
        .expect("numeric app id");
    let progress = std::env::args().any(|arg| arg == "--progress");
    let mut steam = Steam::start();
    let began = Instant::now();
    let mut asked = false;
    while began.elapsed() < Duration::from_secs(90) {
        for event in steam.take() {
            match event {
                Event::Reach(lxb_steam::Reach::Online) if !asked => {
                    if progress {
                        steam.achievement_progress(vec![app_id], 1);
                    } else {
                        steam.achievements(app_id, 1);
                    }
                    asked = true;
                }
                Event::AchievementProgress(heard) => match heard.result {
                    Ok(values) => {
                        for (app, p) in &values {
                            println!(
                                "app={app} unlocked={} total={} cache_time={}",
                                p.unlocked, p.total, p.fetched_at
                            );
                        }
                        println!("{} progress summaries", values.len());
                        return;
                    }
                    Err(reason) => {
                        eprintln!("progress request failed: {reason}");
                        std::process::exit(1);
                    }
                },
                Event::Achievements(heard) => match heard.result {
                    Ok(snapshot) => {
                        println!("app={app_id} total={} unlocked={} icons={} hidden={} percentages={} cached={}",
                            snapshot.achievements.len(), snapshot.achievements.iter().filter(|a| a.achieved).count(),
                            snapshot.achievements.iter().filter(|a| a.icon.is_some() && a.icon_gray.is_some()).count(),
                            snapshot.achievements.iter().filter(|a| a.hidden).count(), snapshot.percentages.len(), snapshot.stale.is_some());
                        if let Some(reason) = snapshot.stale {
                            eprintln!("cached result: {reason}");
                            std::process::exit(1);
                        }
                        return;
                    }
                    Err(reason) => {
                        eprintln!("achievement request failed: {reason}");
                        std::process::exit(1);
                    }
                },
                Event::SignedOut => {
                    eprintln!("No stored Steam session.");
                    std::process::exit(1);
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("Timed out waiting for Steam.");
    std::process::exit(1);
}
