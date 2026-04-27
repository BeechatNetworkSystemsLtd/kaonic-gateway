use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Title};
use leptos_router::{
    components::{Route, Router, Routes},
    StaticSegment,
};

use crate::components::navbar::Navbar;
use crate::pages::{
    dashboard::DashboardPage, media::MediaPage, network::NetworkPage, plugins::PluginsPage,
    radio::RadioPage, reticulum::ReticulumPage, update::SystemPage, vpn::VpnPage,
};

const VPN_SHORTCUT_REDIRECT_JS: &str = r#"
(function() {
    try {
        var params = new URLSearchParams(window.location.search || '');
        if (!params.has('vpn-add-peer')) { return; }
        if (window.location.pathname === '/vpn') { return; }
        window.location.replace('/vpn' + (window.location.search || '') + (window.location.hash || ''));
    } catch (_) {}
})();
"#;

pub fn shell(options: leptos::config::LeptosOptions) -> impl IntoView {
    let _ = options;
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <MetaTags/>
                <link rel="stylesheet" href="/style.css"/>
            </head>
            <body>
                <App/>
                <script>{VPN_SHORTCUT_REDIRECT_JS}</script>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Title text="Kaonic Gateway"/>
        <Router>
            <Navbar/>
            <main class="main-content">
                <Routes fallback=|| view! { <p class="not-found">"Page not found."</p> }>
                    <Route path=StaticSegment("") view=DashboardPage/>
                    <Route path=StaticSegment("radio") view=RadioPage/>
                    <Route path=StaticSegment("reticulum") view=ReticulumPage/>
                    <Route path=StaticSegment("vpn") view=VpnPage/>
                    <Route path=StaticSegment("plugins") view=PluginsPage/>
                    <Route path=StaticSegment("settings") view=RadioPage/>
                    <Route path=StaticSegment("network") view=NetworkPage/>
                    <Route path=StaticSegment("media") view=MediaPage/>
                    <Route path=StaticSegment("system") view=SystemPage/>
                    <Route path=StaticSegment("update") view=SystemPage/>
                </Routes>
            </main>
        </Router>
    }
}
