//! The `art` URL scheme, which serves artwork images to the window (`http://art.localhost/...` in
//! WebView2). It serves only these two kinds of path:
//!
//! - `/source/<slot>/<URL-encoded game key>`: steambro's source image for the slot, fetched first
//!   if steambro doesn't have it yet. The window crops it itself, the way Steam will show it.
//! - `/steam/<slot>/<app id>`: the image in the current account's Steam grid folder.

use percent_encoding::percent_decode_str;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use super::Shared;
use crate::art::{Slot, render};
use crate::steam::Steam;
use crate::sync;

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    let path = request.uri().path().to_string();
    // A fetch can take a while, so it runs on its own thread and the window's other requests don't wait.
    std::thread::spawn(move || {
        let found = image(&app.state::<Shared>(), &path);
        let response = match found {
            Some(bytes) => Response::builder()
                .header(header::CONTENT_TYPE, render::mime(&bytes))
                // Images change when the user edits them; the window adds a version to the URL too.
                .header(header::CACHE_CONTROL, "no-store")
                .body(bytes),
            None => Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()),
        };
        responder.respond(response.expect("static headers are valid"));
    });
}

fn image(shared: &Shared, path: &str) -> Option<Vec<u8>> {
    let parts: Vec<&str> = path.trim_start_matches('/').splitn(3, '/').collect();
    let [kind, slot, id] = parts.as_slice() else { return None };
    let slot = Slot::from_name(slot)?;
    match *kind {
        "source" => {
            let library = shared.library(false);
            let game = library.find(&percent_decode_str(id).decode_utf8_lossy())?;
            sync::source(&library, &shared.cache, game, slot).ok().flatten()
        }
        "steam" => {
            let app_id: u32 = id.parse().ok()?;
            let user = Steam::locate().ok()?.current_user().ok()??;
            std::fs::read(sync::existing_file(&user.grid_dir(), app_id, slot)?).ok()
        }
        _ => None,
    }
}

