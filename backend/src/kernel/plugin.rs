#![allow(clippy::doc_markdown)]
#![allow(dead_code)]

use crate::kernel::App;

pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn inject(&self) -> &'static [&'static str] {
        &[]
    }
    fn register(&self, app: &mut App);
}
