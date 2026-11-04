use std::io::ErrorKind;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tmv_core::{demo, ProcessSnapshot};
use tmv_tui::{ui, App};

/// 以 treemap 方式查看进程虚拟地址空间（主要面向 Linux arm64）。
#[derive(Debug, Parser)]
#[command(name = "tmv", version, about)]
struct Args {
    /// 目标进程 PID
    pid: Option<i32>,

    /// 使用合成数据，便于在没有 Linux /proc 的环境下预览界面
    #[arg(long)]
    demo: bool,

    /// 自动采样间隔（秒），0 表示只在按 r 时刷新
    #[arg(long, default_value_t = 0, value_name = "SECONDS")]
    interval: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let privileged = is_privileged();

    let snapshot = if args.demo {
        demo::snapshot()
    } else {
        let Some(pid) = args.pid else {
            bail!("请提供目标进程 PID，或使用 --demo 预览界面");
        };
        ProcessSnapshot::collect(pid).map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow!("{err}")
            } else {
                anyhow!("无法读取 /proc/{pid}（需要同 uid 或 root 权限）: {err}")
            }
        })?
    };

    let mut terminal = ratatui::init();
    let mut app = App::new(snapshot, args.pid, args.demo, privileged);
    if args.interval > 0 {
        app.set_interval(Some(Duration::from_secs(args.interval)));
    }
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }

        app.tick();
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid 只读取进程属性。
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
