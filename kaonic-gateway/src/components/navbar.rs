use leptos::prelude::*;
use leptos_router::components::A;

async fn fetch_serial() -> Result<String, ServerFnError> {
    use crate::state::AppState;
    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState"))?;
    Ok(state.serial.clone())
}

#[component]
pub fn Navbar() -> impl IntoView {
    let serial = Resource::new(|| (), |_| fetch_serial());

    view! {
        <header class="topbar">
            <div class="topbar-brand">
                <img src="/kaonic-logo.svg" alt="Kaonic" class="topbar-logo-img"/>
            </div>
            <nav class="topbar-nav">
                <A href="/" exact=true attr:class="nav-link">"Dashboard"</A>
                <A href="/settings" attr:class="nav-link">"Radio"</A>
                <A href="/network" attr:class="nav-link">"Network"</A>
                <A href="/media" attr:class="nav-link">"Media"</A>
                <A href="/system" attr:class="nav-link">"System"</A>
            </nav>
            <div class="topbar-serial">
                <Suspense fallback=|| ()>
                    {move || serial.get().and_then(|r| r.ok()).map(|s| view! {
                        <span class="serial-label">"SN"</span>
                        <code class="serial-value">{s}</code>
                    })}
                </Suspense>
            </div>
        </header>
    }
}
