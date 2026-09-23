#[cfg(target_os = "mochios")]
mod api;
#[cfg(not(target_os = "mochios"))]
#[path = "preview_api.rs"]
mod api;
mod catalog;

use api::ApiError;
use catalog::{CatalogApp, Storefront, StorefrontSection};
use viewkit::prelude::*;
use viewkit::view::{Constraints, MeasureContext, PaintContext};

const LIBRARY: [&str; 2] = ["Updates", "Installed"];
const APP_ICON_RADIUS: f32 = 14.0;

type StoreView = NavigationSplitView<Sidebar<VStack>, VStack>;

fn navigation(storefront: Option<&Storefront>, search: State<String>) -> Sidebar<VStack> {
    let mut browse = List::new().row(
        ListRow::new("All Apps")
            .icon(SymbolName::Grid)
            .selected(true),
    );
    if let Some(storefront) = storefront {
        for category in &storefront.categories {
            browse = browse.row(ListRow::new(category.name.clone()).icon(SymbolName::Tag));
        }
    }
    let library = LIBRARY
        .iter()
        .map(|label| match *label {
            "Updates" => ListRow::new(*label).icon(SymbolName::ArrowDownCircle),
            "Installed" => ListRow::new(*label).icon(SymbolName::Internaldrive),
            _ => ListRow::new(*label),
        });

    Sidebar::new(
        VStack::new()
            .alignment(StackAlignment::Stretch)
            .gap(StackGap::Medium)
            .child(Text::styled("App Store", TextRole::TitleSmall).weight(600))
            .child(
                TextField::new(search.binding())
                    .placeholder("Search apps")
                    .size(TextFieldSize::Small)
                    .leading_symbol(SymbolName::Search)
                    .frame(Theme::current().layout.compact_form_control_width, Theme::current().layout.control_height),
            )
            .child(Text::metadata("Explore"))
            .child(browse)
            .child(Text::metadata("Library"))
            .child(List::new().rows(library))
            .child(Spacer::new())
            .child(Text::metadata("mochiOS")),
    )
}

struct AppIcon {
    initial: String,
    size: f32,
}

impl View for AppIcon {
    fn measure(&self, constraints: Constraints, _context: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(Size::new(self.size, self.size))
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        Rectangle::new()
            .color(RectangleColor::Accent)
            .radius(CornerRadius::Custom(APP_ICON_RADIUS))
            .paint(bounds, context);
        Text::styled(self.initial.clone(), TextRole::TitleMedium)
            .weight(700)
            .alignment(TextAlignment::Center)
            .color(Color::WHITE)
            .paint(
                Rect::new(
                    bounds.origin.x,
                    bounds.origin.y + bounds.size.height * 0.18,
                    bounds.size.width,
                    bounds.size.height * 0.7,
                ),
                context,
            );
    }
}

fn app_icon(app: &CatalogApp, size: f32) -> AppIcon {
    AppIcon {
        initial: app.name.chars().next().unwrap_or('A').to_string(),
        size,
    }
}

fn app_row(app: &CatalogApp) -> Padding<HStack> {
    let description = app
        .subtitle
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(&app.description);
    let subtitle = if description.is_empty() {
        format!("{}  ·  {}", app.developer, app.bundle_id)
    } else {
        format!("{}  ·  {}", app.developer, description)
    };

    Padding::symmetric(8.0, 9.0).content(
        HStack::new()
            .alignment(StackAlignment::Center)
            .gap(StackGap::Medium)
            .child(app_icon(app, 48.0))
            .child(
                VStack::new()
                    .alignment(StackAlignment::Stretch)
                    .gap(StackGap::ExtraSmall)
                    .child(Text::label(app.name.clone()).weight(600))
                    .child(Text::caption(subtitle).tone(TextTone::Secondary))
                    .layout()
                    .flex_grow(1.0),
            )
            .child(
                Button::new("Get")
                    .style(ButtonStyle::Standard)
                    .size(ButtonSize::Small)
                    .enabled(false),
            ),
    )
}

fn app_matches_search(app: &CatalogApp, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || app.name.to_ascii_lowercase().contains(&query)
        || app.developer.to_ascii_lowercase().contains(&query)
        || app.description.to_ascii_lowercase().contains(&query)
        || app.subtitle.as_deref().unwrap_or("").to_ascii_lowercase().contains(&query)
        || app.bundle_id.to_ascii_lowercase().contains(&query)
}

fn section_view(section: &StorefrontSection, query: &str) -> VStack {
    let mut heading = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::ExtraSmall)
        .child(Text::styled(section.title.clone(), TextRole::TitleMedium));
    if let Some(subtitle) = section
        .subtitle
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        heading = heading.child(Text::body(subtitle).tone(TextTone::Secondary));
    }

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(heading)
        .child(List::new().rows(
            section.apps.iter().filter(|app| app_matches_search(app, query)).map(app_row),
        ))
}

fn catalog_content(storefront: &Storefront, query: &str) -> VStack {
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::DoubleExtraLarge)
        .child(
            PageHeader::new(if query.trim().is_empty() {
                "Apps"
            } else {
                "Search Results"
            })
            .subtitle(if query.trim().is_empty() {
                "Apps available for mochiOS."
            } else {
                "Apps matching your search."
            }),
        );

    let mut app_count = 0usize;
    for section in &storefront.sections {
        if section.apps.is_empty() {
            continue;
        }
        let matches = section.apps.iter().filter(|app| app_matches_search(app, query)).count();
        if matches == 0 {
            continue;
        }
        app_count += matches;
        content = content.child(section_view(section, query));
    }

    if app_count == 0 {
        content = content
            .child(Text::styled(if query.trim().is_empty() {
                "No apps are currently available"
            } else {
                "No matching apps"
            }, TextRole::TitleMedium))
            .child(
                Text::body(if query.trim().is_empty() {
                    "Published apps will appear here when they become available."
                } else {
                    "Try a different name or developer."
                })
                    .tone(TextTone::Secondary),
            );
    }
    content
}

fn error_content(error: &ApiError) -> VStack {
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(
            PageHeader::new("App Store is unavailable").subtitle(
                "The catalog could not be loaded. Check the network connection and reopen App Store.",
            ),
        )
        .child(Text::metadata(error.to_string()))
}

fn detail(catalog: &Result<Storefront, ApiError>, search: State<String>) -> VStack {
    let content = match catalog {
        Ok(storefront) => catalog_content(storefront, &search.get()),
        Err(error) => error_content(error),
    };

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::None)
        .child(
            Scroll::vertical(ContentArea::new(content))
                .layout()
                .flex_grow(1.0),
        )
}

struct AppStoreApp {
    catalog: Result<Storefront, ApiError>,
    search: State<String>,
}

impl App for AppStoreApp {
    type Body = StoreView;

    fn new() -> Self {
        Self {
            catalog: api::fetch_storefront(),
            search: State::new(String::new()),
        }
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("App Store")
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        let storefront = self.catalog.as_ref().ok();
        NavigationSplitView::new(
            navigation(storefront, self.search.clone()),
            detail(&self.catalog, self.search.clone()),
        )
    }
}

fn main() -> Result<(), ViewKitError> {
    viewkit::run::<AppStoreApp>()
}

#[cfg(test)]
mod tests {
    use super::{CatalogApp, app_matches_search};

    #[test]
    fn catalog_search_matches_visible_app_details() {
        let app = CatalogApp {
            bundle_id: String::from("org.mochios.notes"),
            name: String::from("Notes"),
            version: String::from("1.0"),
            developer: String::from("mochiOS"),
            description: String::from("Write ideas"),
            subtitle: Some(String::from("Quick capture")),
        };
        assert!(app_matches_search(&app, "notes"));
        assert!(app_matches_search(&app, "QUICK"));
        assert!(app_matches_search(&app, "write"));
        assert!(!app_matches_search(&app, "calendar"));
    }
}
