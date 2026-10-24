//! Writes a helper once and generates its three forms.

/// Defines each helper three times: for sync capabilities, for async ones,
/// and for async ones whose futures are `Send` (multithreaded runtimes).
/// Rust cannot be generic over sync and async, or over whether a future is
/// `Send`, so seam's capability traits come in those three forms and so do
/// helpers built on them.
///
/// In the body, these perform an effect in the form being generated:
///
/// - `run!(handle, op)`: `Perform`, `AsyncPerform` or `SendPerform`.
/// - `ask!(handle, query)`: `Inspect`, `AsyncInspect` or `SendInspect`.
/// - `acquire!(c, acquisition, |c, resource| body)`: `acquire`, or
///   `acquire_async` or `acquire_send` with an async closure.
/// - `lend!(c, acquisition, |c, resource| body)`: the same for `lend`.
/// - `call!(sync, async, send; (args))`: another helper, in the same form.
#[allow(unused_macros)]
macro_rules! three_ways {
    ($(
        $(#[$m:meta])*
        pub fn $name:ident, $aname:ident, $sname:ident
        [$($g:tt)*] ($($args:tt)*) -> $ret:ty
        where
            sync { $($sb:tt)* }
            async { $($ab:tt)* }
            send { $($nb:tt)* }
        $body:block
    )*) => {$(
        $(#[$m])*
        #[allow(clippy::needless_lifetimes)]
        pub fn $name<$($g)*>($($args)*) -> $ret where $($sb)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { $crate::Perform::perform($c, $op) };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { $crate::Inspect::inspect($c, $q) };
            }
            #[allow(unused_macros)]
            macro_rules! acquire {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::Provide::acquire($c, $a, |$cc, $r| $b)
                };
            }
            #[allow(unused_macros)]
            macro_rules! lend {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::Lend::lend($c, $a, |$cc, $r| $b)
                };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $s $xs };
            }
            $body
        }

        #[doc = concat!("`", stringify!($name), "`, for async capabilities.")]
        #[allow(clippy::needless_lifetimes)]
        pub async fn $aname<$($g)*>($($args)*) -> $ret where $($ab)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { $crate::future::AsyncPerform::perform_async($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { $crate::future::AsyncInspect::inspect_async($c, $q).await };
            }
            #[allow(unused_macros)]
            macro_rules! acquire {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::AsyncProvide::acquire_async($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! lend {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::AsyncLend::lend_async($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $as $xs.await };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($name), "`, for async capabilities, with a `Send` future: ",
            "for multithreaded runtimes."
        )]
        #[allow(clippy::needless_lifetimes)]
        pub async fn $sname<$($g)*>($($args)*) -> $ret where $($nb)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { $crate::future::SendPerform::perform_send($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { $crate::future::SendInspect::inspect_send($c, $q).await };
            }
            #[allow(unused_macros)]
            macro_rules! acquire {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::SendScope::acquire_send($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! lend {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    $crate::SendScope::lend_send($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $ss $xs.await };
            }
            $body
        }
    )*};
}
#[allow(unused_imports)]
pub(crate) use three_ways;
