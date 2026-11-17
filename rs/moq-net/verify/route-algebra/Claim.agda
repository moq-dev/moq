--------------------------------------------------------------------------------
-- Claims across trust boundaries: the same algebra at the grain of the nodes
-- that hand a claim on.
--
-- A claim's route is its draining, penalty, and cost from the publisher, and the
-- path of nodes that handed it on. A link here is a step a claim takes: from an
-- endpoint to the relay announcing it, from a node to the claim of an announcer
-- in its routing domain over the node's settled route there, or across a scoped
-- grant. Each step only adds: it may make the claim draining, raise its penalty,
-- add cost, and prepends the node that took it, which refuses a claim that names
-- it already. How Rust's claims map onto this, and where Rust's chain names fewer
-- nodes than this path, is in this directory's `README.md`.
--------------------------------------------------------------------------------

module Claim where

open import Data.Bool.Base using (Bool; true; false; not)
open import Data.Nat.Base using (ℕ)
open import Data.Unit.Base using (⊤; tt)
import Data.Unit.Properties as Unit
open import Relation.Binary.PropositionalEquality using (_≡_; refl)

open import RoutingLib.Routing.Basics.Path.Uncertified.Properties using (≤ₗₑₓ-isDecTotalOrder)

open import Order

private
  OE : Ordered ⊤
  OE = record { _≤_ = _≡_ ; isDecTotalOrder = Unit.≡-isDecTotalOrder }

  -- Among equal ranks and lengths, by the nodes on the path, nearest first.
  OP = record { _≤_ = _ ; isDecTotalOrder = ≤ₗₑₓ-isDecTotalOrder }

open import Increasing OE tt (λ _ → refl) OP public

-- One step a claim can take.
record Step : Set where
  constructor step
  field
    up      : Bool
    cost    : ℕ
    penalty : ℕ
    drains  : Bool

private
  module S = Step

policy : Policy
policy = record
  { Link         = Step
  ; down         = step false 0 0 false
  ; cost         = S.cost
  ; penalty      = S.penalty
  ; drains       = S.drains
  ; marks        = λ _ _ _ → false
  ; marked       = λ _ → false
  ; refuses      = λ f _ _ → not (S.up f)
  ; down-refuses = λ _ _ → refl
  ; tag          = λ _ _ → tt
  }

open Rule policy public
