--------------------------------------------------------------------------------
-- The route rule with nodes named as the binding names a `NodeId`
-- (`rs/moq-net/src/route/tests/algebra.rs`): 1000 × marked + relay.
--------------------------------------------------------------------------------

module Named where

open import Data.Nat.Base using (ℕ; _%_; _/_)

relay mark : ℕ → ℕ
relay n = n % 1000
mark  n = n / 1000

open import Route relay mark public
