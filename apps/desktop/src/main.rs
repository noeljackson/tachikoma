mod backend;
mod components;
mod design;
mod icons;
mod state;
mod view;

use anyhow::{Context, Result};
use clap::Parser;
use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(name = "tachikoma-gui", about = "A local decision system")]
struct Args {
    #[arg(long, value_enum)]
    appearance: Option<design::Appearance>,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    credential: Option<PathBuf>,
    /// Open an offline, read-only example; never contacts the approval service.
    #[arg(long)]
    demo: bool,
    #[arg(long, hide = true, requires = "demo")]
    smoke_test: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let state = dirs::state_dir()
        .context("state directory unavailable")?
        .join("tachikoma");
    let runtime_directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.join("run"));
    let backend = backend::Backend {
        socket: args
            .socket
            .unwrap_or_else(|| runtime_directory.join("tachikoma/tachikoma.sock")),
        credential: args
            .credential
            .unwrap_or_else(|| state.join("approval/desktop.token")),
        demo: args.demo,
    };
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let preferences_path = state.join("desktop/appearance.json");
    gpui_platform::application()
        .with_assets(icons::assets()?)
        .run(move |cx: &mut App| {
            cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
            view::bind_keys(cx);
            if args.smoke_test {
                cx.observe_keystrokes(|event, _, _| eprintln!("Smoke keystroke: {event:?}"))
                    .detach();
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
            let result = cx.open_window(
                WindowOptions::new()
                    .window_bounds(Some(WindowBounds::Windowed(bounds)))
                    .window_min_size(Some(size(px(1000.), px(620.))))
                    .app_id(Some("dev.noel.tachikoma".to_owned()))
                    .titlebar(Some(TitlebarOptions {
                        title: Some("tachikoma".into()),
                        ..Default::default()
                    })),
                |window, cx| {
                    cx.new(|cx| {
                        view::Desktop::new(
                            backend,
                            runtime,
                            args.appearance,
                            preferences_path,
                            window,
                            cx,
                        )
                    })
                },
            );
            let handle = match result {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("Unable to open decision window: {error}");
                    std::process::exit(1);
                }
            };
            cx.activate(true);
            if args.smoke_test {
                let _ = handle.update(cx, |_, window, _| {
                    window.on_next_frame(|_, cx| {
                        eprintln!("Decision UI first frame rendered");
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .timer(Duration::from_secs(12))
                                .await;
                            cx.update(|cx| {
                                eprintln!("Decision UI native smoke test complete");
                                cx.quit();
                            });
                        })
                        .detach();
                    });
                });
            }
        });
    Ok(())
}
