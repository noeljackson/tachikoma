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
fn missing_permissions_keep_menu_order_without_synthesizing_actions() {
    let full = crate::backend::demo_review();
    let labels = |review: &Review, effect: &Effect| {
        permission_options(review, effect)
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>()
    };
    let mut limited = full.clone();
    limited.request.subject["process"] = serde_json::json!("");
    limited
        .actions
        .retain(|action| action.scope["duration"] == "once");
    for effect in [Effect::Permit, Effect::Block] {
        assert_eq!(labels(&full, &effect), labels(&limited, &effect));
        let options = permission_options(&limited, &effect);
        assert_eq!(options.len(), 7);
        assert!(options[0].action.is_some());
        assert!(options[1..].iter().all(|option| option.action.is_none()));
        assert_eq!(permission_choices(&limited, &effect).len(), 1);
        assert!(
            permission_unavailable_reason(&limited, &effect)
                .unwrap()
                .contains("executable path")
        );
        assert!(permission_unavailable_reason(&full, &effect).is_none());
    }
    // Familiar action IDs must never turn an unexpected scope into an enabled option.
    limited.actions[0].scope = serde_json::json!({"duration":"forever","match":"unknown"});
    assert!(permission_choices(&limited, &Effect::Permit).is_empty());
    assert_eq!(permission_choices(&limited, &Effect::Block).len(), 1);
    limited.request.subject["process"] = serde_json::json!("/usr/bin/node");
    assert!(
        permission_unavailable_reason(&limited, &Effect::Permit)
            .unwrap()
            .contains("service")
    );
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
        app.update_window(handle.into(), |view, window, cx| {
            view.downcast::<Desktop>()
                .unwrap()
                .update(cx, |desktop, cx| {
                    desktop.state.confirmation = None;
                    let review = desktop.state.snapshot.pending.first_mut().unwrap();
                    review.request.subject["process"] = serde_json::json!("");
                    review
                        .actions
                        .retain(|action| action.scope["duration"] == "once");
                    review.request.action_ids = review
                        .actions
                        .iter()
                        .map(|action| action.id.clone())
                        .collect();
                    desktop.open_permission_menu(Effect::Permit, window, cx);
                });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        assert!(
            app.debug_bounds(handle.into(), "permission-unavailable-reason")
                .unwrap()
                .is_some()
        );
        app.capture_screenshot(handle.into())
            .unwrap()
            .save(output.join(format!("{theme}-limited-permissions.png")))
            .unwrap();
        let disabled = app
            .debug_bounds(handle.into(), "permission-option-3")
            .unwrap()
            .unwrap();
        app.update_window(handle.into(), |view, window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    position: disabled.center(),
                    button: MouseButton::Left,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    position: disabled.center(),
                    button: MouseButton::Left,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
            view.downcast::<Desktop>()
                .unwrap()
                .update(cx, |desktop, _| {
                    assert!(
                        desktop.state.confirmation.is_none(),
                        "disabled scope must ignore clicks"
                    );
                    assert!(desktop.permission_menu.is_some());
                });
        })
        .unwrap();
    }
}
