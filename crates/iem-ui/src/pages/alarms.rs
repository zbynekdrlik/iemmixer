//! The owner's one-time alarm link (`/alarms?t=<token>`, S6 bootstrap): one
//! button asks for notifications, subscribes this phone to Web Push and
//! registers it with the link's token as an alarm recipient
//! (`POST /api/alarms/subscribe`). The server takes a link once.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

const NO_PUSH: &str = "Tento prehliadač nevie prijímať upozornenia.";
const DONE: &str = "Hotovo: tento telefón dostane upozornenia.";

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Ready,
    Working,
    Done,
    Failed(String),
}

/// The page's text for a refused subscription (`status` = HTTP status).
pub fn subscribe_error_message(status: u16) -> String {
    match status {
        400 => "Prehliadač neposlal úplné údaje pre upozornenia.".to_string(),
        403 => "Odkaz je neplatný, vypršal alebo už bol použitý. Požiadaj o nový.".to_string(),
        other => format!("Povolenie zlyhalo ({other})."),
    }
}

/// Whether the answer of `Notification.requestPermission()` allows push.
pub fn permission_granted(answer: Option<&str>) -> bool {
    answer == Some("granted")
}

/// The VAPID public key in a `GET /api/push/vapid-key` answer.
pub fn vapid_key_of(json: &serde_json::Value) -> Option<String> {
    json.get("key")
        .and_then(|k| k.as_str())
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

#[component]
pub fn AlarmsPage() -> impl IntoView {
    let query = use_query_map();
    let token = move || query.get_untracked().get("t").filter(|t| !t.is_empty());
    let has_token = token().is_some();
    let (step, set_step) = signal(Step::Ready);

    let enable = move |_| {
        let Some(t) = token() else {
            return;
        };
        // Asked within the click: browsers grant notifications to a gesture.
        let permission = web_sys::Notification::request_permission();
        let _ = set_step.try_set(Step::Working);
        wasm_bindgen_futures::spawn_local(async move {
            let next = match enable_alarms(permission, &t).await {
                Ok(()) => Step::Done,
                Err(msg) => Step::Failed(msg),
            };
            let _ = set_step.try_set(next);
        });
    };
    let busy = move || matches!(step.get(), Step::Working | Step::Done);

    view! {
        <div class="app">
            <main class="main">
                <div class="login-container">
                    <div class="login-box">
                        <h2>"Upozornenia iemmixera"</h2>
                        <p class="subtitle">"Tento telefón bude dostávať upozornenia strážcu iemmixera."</p>
                        {if has_token {
                            view! {
                                <button class="btn" data-testid="alarm-enable" disabled=busy on:click=enable>
                                    "Povoliť upozornenia"
                                </button>
                            }.into_any()
                        } else {
                            view! {
                                <p class="login-error">"Odkaz je neúplný. Otvor celý odkaz zo správy."</p>
                            }.into_any()
                        }}
                        {move || match step.get() {
                            Step::Ready => None,
                            Step::Working => Some(view! {
                                <p class="subtitle" data-testid="alarm-result">"Povoľujem…"</p>
                            }.into_any()),
                            Step::Done => Some(view! {
                                <p class="subtitle" data-testid="alarm-result">{DONE}</p>
                            }.into_any()),
                            Step::Failed(msg) => Some(view! {
                                <p class="login-error" data-testid="alarm-result">{msg}</p>
                            }.into_any()),
                        }}
                    </div>
                </div>
            </main>
        </div>
    }
}

/// A browser step that failed: logged for diagnosis, `NO_PUSH` for the page.
fn failed(what: &str, e: &JsValue) -> String {
    leptos::logging::log!("[alarms] {what}: {e:?}");
    NO_PUSH.to_string()
}

/// The permission answer → the server's VAPID key → a Web Push subscription
/// → `POST /api/alarms/subscribe` with the token. Browser only.
async fn enable_alarms(
    permission: Result<js_sys::Promise, JsValue>,
    token: &str,
) -> Result<(), String> {
    let permission = permission.map_err(|e| failed("Notification.requestPermission", &e))?;
    let answer = JsFuture::from(permission)
        .await
        .map_err(|e| failed("permission answer", &e))?
        .as_string();
    if !permission_granted(answer.as_deref()) {
        return Err(
            "Upozornenia sú v prehliadači zakázané. Povoľ ich pre túto stránku a skús znova."
                .to_string(),
        );
    }
    let key = vapid_key().await?;
    let window = web_sys::window().ok_or_else(|| NO_PUSH.to_string())?;
    let container: web_sys::ServiceWorkerContainer =
        js_sys::Reflect::get(&window.navigator(), &JsValue::from_str("serviceWorker"))
            .map_err(|e| failed("navigator.serviceWorker", &e))?
            .dyn_into()
            .map_err(|e| failed("navigator.serviceWorker", &e))?;
    let ready = container
        .ready()
        .map_err(|e| failed("serviceWorker.ready", &e))?;
    let registration: web_sys::ServiceWorkerRegistration = JsFuture::from(ready)
        .await
        .map_err(|e| failed("serviceWorker.ready", &e))?
        .dyn_into()
        .map_err(|e| failed("service worker registration", &e))?;
    let push = registration
        .push_manager()
        .map_err(|e| failed("pushManager", &e))?;
    let key_bytes = crate::pages::mixer::push::base64url_decode(&key)
        .ok_or_else(|| "Server poslal chybný kľúč pre upozornenia.".to_string())?;
    let key_array = js_sys::Uint8Array::new_with_length(key_bytes.len() as u32);
    key_array.copy_from(&key_bytes);
    let opts = web_sys::PushSubscriptionOptionsInit::new();
    opts.set_user_visible_only(true);
    opts.set_application_server_key(&key_array.into());
    let pending = push
        .subscribe_with_options(&opts)
        .map_err(|e| failed("pushManager.subscribe", &e))?;
    let subscription = JsFuture::from(pending)
        .await
        .map_err(|e| failed("pushManager.subscribe", &e))?;
    // JSON.stringify calls the subscription's toJSON(): {endpoint, keys}.
    let text = js_sys::JSON::stringify(&subscription)
        .map_err(|e| failed("subscription JSON", &e))?
        .as_string()
        .ok_or_else(|| NO_PUSH.to_string())?;
    let subscription: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| NO_PUSH.to_string())?;
    let resp = gloo_net::http::Request::post("/api/alarms/subscribe")
        .json(&serde_json::json!({ "token": token, "subscription": subscription }))
        .map_err(|_| NO_PUSH.to_string())?
        .send()
        .await
        .map_err(|_| "Server je nedostupný. Skús to znova.".to_string())?;
    if resp.ok() {
        Ok(())
    } else {
        Err(subscribe_error_message(resp.status()))
    }
}

/// The server's VAPID public key (`GET /api/push/vapid-key`).
async fn vapid_key() -> Result<String, String> {
    let resp = gloo_net::http::Request::get("/api/push/vapid-key")
        .send()
        .await
        .map_err(|_| "Server je nedostupný. Skús to znova.".to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|_| NO_PUSH.to_string())?;
    vapid_key_of(&json).ok_or_else(|| "Server nemá kľúč pre upozornenia.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_read_as_sentences() {
        assert!(subscribe_error_message(403).starts_with("Odkaz je neplatný"));
        assert!(subscribe_error_message(400).contains("úplné údaje"));
        assert_eq!(subscribe_error_message(500), "Povolenie zlyhalo (500).");
    }

    #[test]
    fn only_granted_allows_push() {
        assert!(permission_granted(Some("granted")));
        assert!(!permission_granted(Some("denied")));
        assert!(!permission_granted(Some("default")));
        assert!(!permission_granted(None));
    }

    #[test]
    fn the_vapid_key_is_read_from_the_answer() {
        assert_eq!(
            vapid_key_of(&serde_json::json!({ "key": "BAbc" })),
            Some("BAbc".to_string())
        );
        assert_eq!(vapid_key_of(&serde_json::json!({ "key": null })), None);
        assert_eq!(vapid_key_of(&serde_json::json!({ "key": "" })), None);
        assert_eq!(vapid_key_of(&serde_json::json!({})), None);
    }
}
