use super::*;
use gpui::{AssetRegistry, AssetSource, HeadlessAppContext, SharedString};
use std::{borrow::Cow, collections::HashSet};

struct Assets(AssetRegistry);
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(self
            .0
            .load(path)
            .map(|bytes| Cow::Owned(bytes.into_owned())))
    }
    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Icon::ALL.iter().map(|icon| icon.path().into()).collect())
    }
}

#[test]
#[ignore = "requires the Docker software GPU and font environment"]
fn permission_layout() {
    let output = PathBuf::from(
        std::env::var_os("TACHIKOMA_RENDER_OUTPUT").expect("render output directory"),
    );
    std::fs::create_dir_all(&output).unwrap();
    for appearance in [Appearance::Light, Appearance::Dark] {
        let platform = gpui_platform::current_platform(true);
        let mut app = HeadlessAppContext::with_platform(
            platform.text_system(),
            Arc::new(Assets(crate::icons::assets().unwrap())),
            || {
                Some(Box::new(
                    gpui_wgpu::WgpuHeadlessRenderer::new().expect("software GPU"),
                ))
            },
        );
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        );
        let backend = Backend {
            demo: true,
            socket: PathBuf::new(),
            credential: PathBuf::new(),
        };
        let snapshot = runtime.block_on(backend.snapshot(None)).unwrap();
        let handle = app
            .open_window(gpui::size(px(1320.), px(860.)), |window, cx| {
                cx.new(|cx| {
                    Desktop::new(
                        backend,
                        runtime.clone(),
                        Some(appearance),
                        output.join("appearance.json"),
                        window,
                        cx,
                    )
                })
            })
            .unwrap();
        app.update_window(handle.into(), |view, window, cx| {
            view.downcast::<Desktop>()
                .unwrap()
                .update(cx, |desktop, cx| {
                    desktop.state.apply(snapshot, None);
                    desktop.open_permission_menu(Effect::Permit, window, cx);
                });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        let theme = if appearance == Appearance::Light {
            "light"
        } else {
            "dark"
        };
        let control = app
            .debug_bounds(handle.into(), "Allow-permission-control")
            .unwrap()
            .unwrap();
        let menu_bounds = app
            .debug_bounds(handle.into(), "permission-menu")
            .unwrap()
            .unwrap();
        assert!(
            menu_bounds.top() >= control.bottom(),
            "dropdown must open below its split button"
        );
        let menu = app.capture_screenshot(handle.into()).unwrap();
        assert!(
            menu.pixels().map(|p| p.0).collect::<HashSet<_>>().len() > 64,
            "menu must contain actual rendered text and controls"
        );
        menu.save(output.join(format!("{theme}-allow-menu.png")))
            .unwrap();
        app.update_window(handle.into(), |view, window, cx| {
            view.downcast::<Desktop>()
                .unwrap()
                .update(cx, |desktop, cx| {
                    let review = desktop.state.review().unwrap().clone();
                    let action = review
                        .actions
                        .iter()
                        .find(|a| a.id == "allow-executable-forever")
                        .unwrap()
                        .clone();
                    desktop.open_confirmation(Confirmation { review, action }, false, window, cx);
                });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        let bounds = app
            .debug_bounds(handle.into(), "permission-confirmation")
            .unwrap()
            .unwrap();
        assert!(
            (f32::from(bounds.center().x) - 660.).abs() < 1.,
            "dialog must center horizontally"
        );
        assert!(
            (f32::from(bounds.center().y) - 430.).abs() < 1.,
            "dialog must center vertically"
        );
        let modal = app.capture_screenshot(handle.into()).unwrap();
        assert_ne!(menu, modal, "modal must replace the open menu");
        modal
            .save(output.join(format!("{theme}-permission-dialog.png")))
            .unwrap();
    }
}
