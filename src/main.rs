mod api;
mod catalog;

use api::ApiError;
use catalog::{CatalogApp, Storefront, StorefrontSection};
use viewkit::prelude::*;

const LIBRARY: [(&str, SymbolName); 2] = [
    ("Updates", SymbolName::ArrowDown),
    ("Installed", SymbolName::Tray),
];

type StoreView = NavigationSplitView<Sidebar<VStack>, VStack>;

fn navigation(storefront: Option<&Storefront>) -> Sidebar<VStack> {
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
        .map(|(label, symbol)| ListRow::new(*label).icon(*symbol));

    Sidebar::new(
        VStack::new()
            .alignment(StackAlignment::Stretch)
            .gap(StackGap::Small)
            .child(Text::metadata("mochiOS"))
            .child(Text::styled("App Store", TextRole::TitleLarge))
            .child(Text::metadata("BROWSE"))
            .child(browse)
            .child(Text::metadata("LIBRARY"))
            .child(List::new().rows(library))
            .child(Spacer::new())
            .child(Text::metadata("Catalog provided by api.store.mochios.org")),
    )
}

fn toolbar() -> Toolbar<HStack> {
    Toolbar::new(
        HStack::new()
            .alignment(StackAlignment::Center)
            .child(
                TextField::with_interaction(TextFieldInteractionState::new())
                    .placeholder("Search the App Store")
                    .capsule()
                    .layout()
                    .flex_grow(1.0),
            ),
    )
}

fn app_row(app: &CatalogApp) -> ListRow {
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

    ListRow::new(app.name.clone())
        .subtitle(subtitle)
        .trailing(app.version.clone())
}

fn section_view(section: &StorefrontSection) -> VStack {
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
        .child(Card::new().content(List::new().rows(section.apps.iter().map(app_row))))
}

fn catalog_content(storefront: &Storefront) -> VStack {
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Large)
        .child(
            VStack::new()
                .alignment(StackAlignment::Stretch)
                .gap(StackGap::ExtraSmall)
                .child(Text::styled("Applications", TextRole::TitleLarge))
                .child(
                    Text::body("Browse apps available for mochiOS.")
                        .tone(TextTone::Secondary),
                ),
        );

    let mut app_count = 0usize;
    for section in &storefront.sections {
        if section.apps.is_empty() {
            continue;
        }
        app_count += section.apps.len();
        content = content.child(section_view(section));
    }

    if app_count == 0 {
        content = content
            .child(Text::styled("No apps are currently available", TextRole::TitleMedium))
            .child(
                Text::body("Published apps will appear here when they become available.")
                    .tone(TextTone::Secondary),
            );
    }
    content
}

fn error_content(error: &ApiError) -> VStack {
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(Text::styled("App Store is unavailable", TextRole::TitleLarge))
        .child(
            Text::body("The catalog could not be loaded. Check the network connection and reopen App Store.")
                .tone(TextTone::Secondary),
        )
        .child(Text::metadata(error.to_string()))
}

fn detail(catalog: &Result<Storefront, ApiError>) -> VStack {
    let content = match catalog {
        Ok(storefront) => catalog_content(storefront),
        Err(error) => error_content(error),
    };

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::None)
        .child(toolbar())
        .child(
            Scroll::vertical(ContentArea::new(content))
                .layout()
                .flex_grow(1.0),
        )
}

struct AppStoreApp {
    catalog: Result<Storefront, ApiError>,
}

impl App for AppStoreApp {
    type Body = StoreView;

    fn new() -> Self {
        Self {
            catalog: api::fetch_storefront(),
        }
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("App Store")
            .size(1120.0, 760.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        let storefront = self.catalog.as_ref().ok();
        NavigationSplitView::new(navigation(storefront), detail(&self.catalog))
    }
}

fn main() -> Result<(), ViewKitError> {
    viewkit::run::<AppStoreApp>()
}
