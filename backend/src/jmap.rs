//! Legacy module path — re-exports kept until the remaining callers migrate
//! (deleted wholesale in the transport-removal commit).

// `JmapError` is consumed via `crate::jmap::…` by sync/types.rs and jobs.rs;
// the rest re-export for callers that migrate in the transport-removal task.
#[allow(unused_imports)]
pub use crate::sync::jmap_client::{
    JmapEmail, JmapEmailAddress, JmapError, JmapMailbox, decrypt_account_password,
};
