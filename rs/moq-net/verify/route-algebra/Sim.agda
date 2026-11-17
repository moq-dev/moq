--------------------------------------------------------------------------------
-- The route rule as the Rust prototype runs it: a node is named
-- 1000 × start + relay, as `hop` in `rs/moq-net/src/route/tests/mod.rs` names it.
--------------------------------------------------------------------------------

module Sim where

open import Data.Nat.Base using (ℕ; _%_; _/_; _+_; _*_)
open import Data.Nat.DivMod using (m≡m%n+[m/n]*n)
open import Relation.Binary.PropositionalEquality using (_≡_; cong₂; trans; sym)

relay start : ℕ → ℕ
relay n = n % 1000
start n = n / 1000

relay-start-injective : ∀ {m n} → relay m ≡ relay n → start m ≡ start n → m ≡ n
relay-start-injective {m} {n} r s = trans (m≡m%n+[m/n]*n m 1000)
  (trans (cong₂ (λ a b → a + b * 1000) r s) (sym (m≡m%n+[m/n]*n n 1000)))

open import Route relay start relay-start-injective public
