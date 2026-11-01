// Spike dialect: docs and signature live once, alongside the canonical poll body.
macro_rules! wrapper {
    ($(#[doc = $doc:literal])* [$($attr:tt)*]
     $vis:vis fn $poll:ident => $name:ident (&$this:ident, $waiter:ident: &$waiter_ty:ty)
     -> $out:ty $body:block) => {
        wrapper!(@emit [$(#[doc = $doc])*] [$($attr)*] $vis $poll $name
                 [&$this] $this $waiter $waiter_ty, $out $body);
    };
    ($(#[doc = $doc:literal])* [$($attr:tt)*]
     $vis:vis fn $poll:ident => $name:ident (&mut $this:ident, $waiter:ident: &$waiter_ty:ty)
     -> $out:ty $body:block) => {
        wrapper!(@emit [$(#[doc = $doc])*] [$($attr)*] $vis $poll $name
                 [&mut $this] $this $waiter $waiter_ty, $out $body);
    };
    (@emit [$($doc:tt)*] [$($attr:tt)*] $vis:vis $poll:ident $name:ident
     [$($receiver:tt)*] $this:ident $waiter:ident $waiter_ty:ty, $out:ty $body:block) => {
        $($doc)* $($attr)*
        $vis fn $poll($($receiver)*, $waiter: &$waiter_ty) -> ::std::task::Poll<$out> $body

        $($doc)*
        #[cfg(feature = "async")]
        $vis async fn $name($($receiver)*) -> $out {
            ::kio::wait(|waiter| $this.$poll(waiter)).await
        }
    };
}
