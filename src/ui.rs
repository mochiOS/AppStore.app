//! App Store-specific composition of standard ViewKit controls.
use super::*;

pub(super) const SIDEBAR_MINIMUM: f32 = 200.0;
pub(super) const DETAIL_MINIMUM: f32 = 420.0;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum ToolbarAction {
    Back,
}

pub(super) struct CachedPicker {
    options: Vec<(String, String)>,
    selection: String,
    picker: Rc<Picker>,
}

pub(super) fn page_title(selection: &str, query: &str) -> String {
    if !query.trim().is_empty() {
        return "Search Results".into();
    }
    match selection {
        SELECTION_UPDATES => "Updates".into(),
        SELECTION_INSTALLED => "Installed".into(),
        _ => selection
            .strip_prefix("category:")
            .unwrap_or("Discover")
            .into(),
    }
}

fn navigate(
    target: &str,
    search: &State<String>,
    selection: &State<String>,
    selected_app: &State<Option<String>>,
    ui: &StoreUi,
) {
    search.set(String::new());
    ui.search.set_value(String::new());
    ui.browse_scroll.reset();
    selection.set(target.into());
    selected_app.set(None);
}

fn navigation_picker(
    storefront: Option<&Storefront>,
    search: State<String>,
    selection: State<String>,
    selected_app: State<Option<String>>,
    ui: &Rc<StoreUi>,
) -> Rc<Picker> {
    let mut options = vec![("Discover".into(), SELECTION_ALL.into())];
    if let Some(storefront) = storefront {
        options.extend(
            storefront
                .categories
                .iter()
                .map(|category| (category.name.clone(), category_selection(&category.name))),
        );
    }
    options.extend([
        ("Updates".into(), SELECTION_UPDATES.into()),
        ("Installed".into(), SELECTION_INSTALLED.into()),
    ]);
    let current = selection.get();
    if let Some(cached) = ui.compact_picker.borrow().as_ref()
        && cached.options == options
        && cached.selection == current
    {
        return cached.picker.clone();
    }
    let mut picker = Picker::new(page_title(&current, ""));
    for (label, target) in &options {
        let (search, selection, selected_app, target) = (
            search.clone(),
            selection.clone(),
            selected_app.clone(),
            target.clone(),
        );
        // Avoid retaining StoreUi through a picker cached inside StoreUi itself.
        let weak = Rc::downgrade(ui);
        picker = picker.option(label, move || {
            if let Some(ui) = weak.upgrade() {
                navigate(&target, &search, &selection, &selected_app, &ui);
            }
        });
    }
    let picker = Rc::new(picker);
    *ui.compact_picker.borrow_mut() = Some(CachedPicker {
        options,
        selection: current,
        picker: picker.clone(),
    });
    picker
}

pub(super) fn compact_navigation(
    storefront: Option<&Storefront>,
    search: State<String>,
    selection: State<String>,
    selected_app: State<Option<String>>,
    ui: &Rc<StoreUi>,
) -> VStack {
    let picker = navigation_picker(
        storefront,
        search.clone(),
        selection,
        selected_app.clone(),
        ui,
    );
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(picker)
        .child(StoreSearch {
            field: TextField::with_interaction(ui.search.clone())
                .placeholder("Search Apps")
                .leading_symbol(SymbolName::Search)
                .size(TextFieldSize::Small),
            query: search,
            selected_app,
            scroll: ui.browse_scroll.clone(),
            input: ui.search.clone(),
        })
}

pub(super) struct StoreLayout {
    pub(super) wide: StoreView,
    pub(super) compact: VStack,
}
impl StoreLayout {
    fn content(&self, width: f32) -> &dyn View {
        // Preserve both useful panes at wide widths; do not crush navigation
        // labels into a sliver when the detail needs its normal reading width.
        if width >= SIDEBAR_MINIMUM + DETAIL_MINIMUM {
            &self.wide
        } else {
            &self.compact
        }
    }
}
impl View for StoreLayout {
    fn measure(&self, c: Constraints, context: &mut MeasureContext<'_>) -> Size {
        self.content(c.maximum.width).measure(c, context)
    }
    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        self.content(bounds.size.width).paint(bounds, context);
    }
    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        self.content(bounds.size.width)
            .handle_event(bounds, event, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewkit::{
        accessibility::{AccessibilityNode, AccessibilityRole},
        draw_command::{DisplayList, DrawCommand},
        platform::PointerButton,
        typography::TextMeasurer,
    };

    fn store() -> AppStoreApp {
        let catalog = serde_json::from_str(r#"{"categories":[{"name":"Productivity"}],"sections":[{"title":"Featured","apps":[
            {"bundle_id":"org.example.notes","name":"Notes","version":"1.0.0","developer":"Example","description":"Capture ideas and organize your notes.","subtitle":"Your ideas, together","category":"Productivity"}
        ]}]}"#).unwrap();
        AppStoreApp {
            ui: Rc::new(StoreUi::default()),
            catalog: CatalogStatus::Ready(catalog),
            catalog_revision: State::new(0),
            pending_catalog: None,
            search: State::new(String::new()),
            selection: State::new(SELECTION_ALL.into()),
            selected_app: State::new(None),
            release_status: State::new(ReleaseStatus::Idle),
            install_status: State::new(InstallStatus::Idle),
            pending_release: State::new(None),
            pending_install: State::new(None),
            icons: State::new(Vec::new()),
            pending_icons: Vec::new(),
            installed_packages: State::new(Vec::new()),
            package_query_error: State::new(None),
            setup: vec![],
            setup_attempted: false,
        }
    }
    fn paint(
        app: &AppStoreApp,
        width: f32,
        theme: &Theme,
    ) -> (DisplayList, Vec<AccessibilityNode>) {
        let mut list = DisplayList::new();
        let mut nodes = Vec::new();
        let mut text = TextMeasurer::new();
        let bounds = Rect::new(0.0, 0.0, width, 700.0);
        Surface::app().content(app.store_view()).paint(
            bounds,
            &mut PaintContext::new(&mut list, theme, &theme.typography, &mut text)
                .with_viewport_bounds(bounds)
                .with_accessibility_nodes(&mut nodes),
        );
        (list, nodes)
    }
    fn release() -> CatalogRelease {
        CatalogRelease {
            release_id: "release-1".into(),
            bundle_id: "org.example.notes".into(),
            version: "1.0.0".into(),
            size: 4096,
            sha256: "0".repeat(64),
            architecture: Some("x86_64".into()),
            abi: Some("mochios-1".into()),
        }
    }

    #[test]
    fn pane_corner_exposes_chrome_instead_of_a_second_pane_background() {
        let app = store();
        for theme in [Theme::LIGHT, Theme::DARK] {
            for detail in [false, true] {
                app.selected_app.set(detail.then(|| "org.example.notes".into()));
                let (list, _) = paint(&app, 1040.0, &theme);
                let (index, rect, radii) = list.commands().iter().enumerate().find_map(|(i, cmd)| {
                    match cmd {
                        DrawCommand::PushRoundedCornersClip { rect, radii }
                            if rect.origin.x == 240.0 && rect.origin.y > 0.0 => Some((i, *rect, *radii)),
                        _ => None,
                    }
                }).expect("toolbar/sidebar junction must have a rounded pane");
                assert_eq!(radii, [theme.radius.extra_large, 0.0, 0.0, 0.0]);
                let corner = rect.origin;
                assert!(!viewkit::continuous_corner::shape(rect, radii).contains((corner.x, corner.y)));
                let backdrop = list.commands()[..index].iter().rev().find_map(|cmd| {
                    match cmd {
                        DrawCommand::FillRect { rect, color } if rect.contains(corner) => Some(*color),
                        _ => None,
                    }
                });
                assert_eq!(backdrop, Some(theme.colors.surface_subtle),
                    "the cutout must expose chrome, not a second pane background");
            }
        }
    }

    #[test]
    fn catalog_uses_native_rows_and_switches_to_reachable_compact_navigation() {
        let app = store();
        for theme in [Theme::LIGHT, Theme::DARK] {
            for width in [360.0, 480.0, 620.0, 1040.0] {
                let (_, nodes) = paint(&app, width, &theme);
                assert_eq!(
                    nodes
                        .iter()
                        .filter(|n| n.role == AccessibilityRole::TextField)
                        .count(),
                    1
                );
                assert_eq!(
                    nodes.iter().any(|n| n.role == AccessibilityRole::ComboBox),
                    width < 620.0
                );
                let row = nodes
                    .iter()
                    .find(|n| {
                        n.role == AccessibilityRole::ListItem
                            && n.label.as_deref() == Some("View Notes by Example")
                    })
                    .unwrap();
                assert!(row.bounds.size.width > APP_ICON_SIZE);
                assert!(
                    row.bounds.origin.x >= 0.0
                        && row.bounds.origin.x + row.bounds.size.width <= width + 0.01
                );
            }
        }
    }

    #[test]
    fn compact_menu_survives_reconstruction_and_applies_the_same_library_navigation() {
        let app = store();
        let CatalogStatus::Ready(ref storefront) = app.catalog else {
            unreachable!()
        };
        let picker = navigation_picker(
            Some(storefront),
            app.search.clone(),
            app.selection.clone(),
            app.selected_app.clone(),
            &app.ui,
        );
        app.search.set("notes".into());
        app.ui.search.set_value("notes");
        app.selected_app.set(Some("org.example.notes".into()));
        app.ui.browse_scroll.set_offset(0.0, 300.0);
        let theme = Theme::LIGHT;
        let mut text = TextMeasurer::new();
        let mut context = EventContext::new(&theme, &theme.typography, &mut text);
        let bounds = Rect::new(0.0, 0.0, 300.0, 32.0);
        let position = Point::new(150.0, 16.0);
        for event in [
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
            ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
        ] {
            picker.handle_event(bounds, &event, &mut context);
        }
        let request = context
            .take_context_menu_request()
            .expect("native picker menu");
        let rebuilt = navigation_picker(
            Some(storefront),
            app.search.clone(),
            app.selection.clone(),
            app.selected_app.clone(),
            &app.ui,
        );
        assert!(Rc::ptr_eq(&picker, &rebuilt));
        let item = request
            .items
            .iter()
            .find(|item| item.label == "Installed")
            .unwrap();
        rebuilt.handle_event(
            bounds,
            &ViewEvent::ContextMenuResult {
                request_id: request.request_id,
                command_id: Some(item.command_id),
            },
            &mut context,
        );
        assert_eq!(app.selection.get(), SELECTION_INSTALLED);
        assert!(
            app.search.get().is_empty()
                && app.ui.search.value().is_empty()
                && app.selected_app.get().is_none()
        );
        assert_eq!(app.ui.browse_scroll.offset_y(), 0.0);
    }

    #[test]
    fn detail_uses_real_byte_progress_and_native_action_states() {
        let app = store();
        let release = release();
        app.selected_app.set(Some(release.bundle_id.clone()));
        app.release_status
            .set(ReleaseStatus::Ready(release.clone()));
        app.install_status.set(InstallStatus::Downloading {
            bundle_id: release.bundle_id.clone(),
        });
        app.pending_install.set(Some(PendingInstall {
            request_id: 1,
            release,
            path: "unused-test-path".into(),
            offset: 2048,
            hasher: Sha256::new(),
            mutation: PackageMutation::Install,
        }));
        let (list, nodes) = paint(&app, 480.0, &Theme::LIGHT);
        let progress = nodes
            .iter()
            .find(|n| n.role == AccessibilityRole::ProgressIndicator)
            .unwrap();
        assert_eq!(progress.numeric_value, Some(0.5));
        assert!(
            nodes
                .iter()
                .any(|n| n.label.as_deref() == Some("Downloading") && !n.enabled)
        );
        assert!(list.commands().iter().any(|cmd| matches!(cmd, DrawCommand::DrawText { command } if command.text == "Downloaded 50%")));
        app.pending_install.set(None);
        app.install_status.set(InstallStatus::Failed {
            bundle_id: "org.example.notes".into(),
            message: "Signature verification failed".into(),
        });
        let (list, nodes) = paint(&app, 1040.0, &Theme::DARK);
        assert!(
            !nodes
                .iter()
                .any(|n| n.role == AccessibilityRole::ProgressIndicator)
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.label.as_deref() == Some("Get") && n.enabled)
        );
        assert!(list.commands().iter().any(|cmd| matches!(cmd, DrawCommand::DrawText { command } if command.text == "Signature verification failed")));
    }

    #[test]
    fn standard_back_button_restores_the_existing_browse_position() {
        let app = store();
        app.selected_app.set(Some("org.example.notes".into()));
        app.ui.browse_scroll.set_offset(0.0, 100.0);
        let (_, nodes) = paint(&app, 1040.0, &Theme::LIGHT);
        let back = nodes
            .iter()
            .find(|n| n.label.as_deref() == Some("Back"))
            .unwrap()
            .bounds;
        let position = Point::new(
            back.origin.x + back.size.width * 0.5,
            back.origin.y + back.size.height * 0.5,
        );
        let mut text = TextMeasurer::new();
        let theme = Theme::LIGHT;
        let mut context = EventContext::new(&theme, &theme.typography, &mut text);
        let bounds = Rect::new(0.0, 0.0, 1040.0, 700.0);
        for event in [
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            },
            ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            },
        ] {
            app.store_view().handle_event(bounds, &event, &mut context);
        }
        assert!(app.selected_app.get().is_none());
        assert_eq!(app.ui.browse_scroll.offset_y(), 100.0);
    }

    #[test]
    fn initial_setup_requires_an_actual_installed_package_record() {
        let mut app = store();
        app.setup = vec![setup::Application {
            bundle_id: "org.example.notes".into(),
            name: "Notes".into(),
        }];
        assert!(app.setup_banner().is_some());
        let (_, nodes) = paint(&app, 1040.0, &Theme::LIGHT);
        assert!(
            nodes
                .iter()
                .any(|node| node.label.as_deref() == Some("View in Store") && node.enabled)
        );
        app.installed_packages.set(vec![InstalledPackage {
            package_id: "org.example.notes".into(),
            name: "Notes".into(),
            version: "1.0.0".into(),
            kind: "application".into(),
            built_in: false,
            removable: true,
        }]);
        assert!(app.setup_banner().is_none());
        app.installed_packages.set(Vec::new());
        app.setup[0].bundle_id = "org.example.unpublished".into();
        let (_, nodes) = paint(&app, 1040.0, &Theme::LIGHT);
        assert!(
            nodes
                .iter()
                .any(|node| node.label.as_deref() == Some("Not available yet") && !node.enabled)
        );
    }

    #[test]
    fn protected_packages_and_unavailable_releases_do_not_gain_mutation_actions() {
        let app = store();
        app.selected_app.set(Some("org.example.notes".into()));
        app.installed_packages.set(vec![InstalledPackage {
            package_id: "org.example.notes".into(),
            name: "Notes".into(),
            version: "1.0.0".into(),
            kind: "application".into(),
            built_in: true,
            removable: false,
        }]);
        app.release_status.set(ReleaseStatus::Ready(release()));
        let (_, nodes) = paint(&app, 480.0, &Theme::LIGHT);
        assert!(!nodes.iter().any(|n| n.label.as_deref() == Some("Remove")));
        assert!(
            nodes
                .iter()
                .any(|n| n.label.as_deref() == Some("Installed") && !n.enabled)
        );
        app.installed_packages.set(Vec::new());
        app.release_status.set(ReleaseStatus::Unavailable {
            bundle_id: "org.example.notes".into(),
        });
        let (_, nodes) = paint(&app, 480.0, &Theme::LIGHT);
        assert!(
            nodes
                .iter()
                .any(|n| n.label.as_deref() == Some("Get") && !n.enabled)
        );
    }
}
