use askama::Template;
use axum::response::Html;

use crate::store::ProposalRecord;

#[derive(Template)]
#[template(path = "dashboard.html")]
struct Dashboard {
    proposals: Vec<ProposalRecord>,
    csrf_token: String,
}

/// Server-rendered, progressively-enhanceable local dashboard.
pub fn dashboard(proposals: Vec<ProposalRecord>, csrf_token: String) -> Html<String> {
    Html(
        Dashboard {
            proposals,
            csrf_token,
        }
        .render()
        .expect("dashboard template is valid at compile time"),
    )
}
