//! Arena handles that replace Neovim's raw `curbuf`/`curwin`/`curtab` pointers.
//!
//! In the C codebase, a `buf_T *` becomes dangling when a buffer is wiped, and
//! a huge amount of manual bookkeeping exists to avoid use-after-free. Here we
//! use small `Copy` integer handles resolved through the owning arena, so a
//! stale handle is a clean `None` lookup rather than undefined behavior.

macro_rules! define_handle {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            /// The raw numeric id, as exposed to Lua/RPC (Neovim uses 1-based
            /// handles on the wire; the mapping is the arena's concern).
            #[inline]
            pub fn raw(self) -> u32 {
                self.0
            }
        }

        impl From<u32> for $name {
            #[inline]
            fn from(v: u32) -> Self {
                $name(v)
            }
        }
    };
}

define_handle!(
    /// Identifies a buffer within the editor's buffer arena.
    BufferId
);
define_handle!(
    /// Identifies a window within the editor's window arena.
    WindowId
);
define_handle!(
    /// Identifies a tabpage within the editor's tabpage arena.
    TabpageId
);
