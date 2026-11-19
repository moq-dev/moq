--------------------------------------------------------------------------------
-- The route rule with nodes named as the binding names a `NodeId`
-- (`rs/moq-net/src/route/tests/algebra.rs`): 1000 × (2 × start + marked) + relay.
--------------------------------------------------------------------------------

module Named where

open import Data.Nat.Base using (ℕ; _%_; _/_)

relay start mark : ℕ → ℕ
relay n = n % 1000
start n = n / 2000
mark  n = (n / 1000) % 2

open import Route relay start mark public
