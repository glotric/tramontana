//! Crate-internal boilerplate macros.

/// `new()` for a type whose fresh value is its `Default`, with `$doc` as its doc line.
macro_rules! default_new {
    ($doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }
    };
}

/// A `Debug` that prints only the type name, so no field (secret, port, or
/// large value) reaches a log.
macro_rules! opaque_debug {
    ($($ty:ident),+ $(,)?) => {$(
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    )+};
}
