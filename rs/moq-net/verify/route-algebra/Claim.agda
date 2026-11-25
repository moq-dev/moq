--------------------------------------------------------------------------------
-- Claims across trust boundaries: the same algebra at the grain of the relays
-- that hand a claim on, as `route::relay::Router` ranks and passes claims.
--
-- The destination is a broadcast: one Instance at one path. A claim filed at a
-- prefix of the path reaches it lowered, as a request there sees it, so the claims
-- of one prefix tie on precedence wherever each is filed and rank as one tier
-- (`Router::tier`). A claim's route to it carries the claim's precedence, its draining, penalty, and cost, whether a
-- marked link reached it, the path of relays that handed it on, and whom its
-- holder files it under. A step is one of:
--
-- * the publisher's own, from the publisher to the broadcast, which sets its run;
-- * an adoption: a relay takes the claim from an endpoint or across a scoped
--   grant (`Claim::adopted`), after its giver told it (`Claim::exported`), which
--   adds the link's cost and priority, marks it where the relay marked the link,
--   lowers it where it is shown below its own path, and refuses one the giver
--   marked the link against, as the giver tells it no such claim, and one whose
--   path reaches the longest chain; the relay files it under itself, or under its
--   marked identity once a marked link reached it;
-- * a domain step: a relay takes the claim of an announcer in its routing domain
--   over its settled route there (`Claim::over`).
--
-- Precedence leads: a claim exactly at its path before a broader one shown there,
-- then the newer run. Every step into a node refuses a claim below the best
-- precedence the node knows of, reachable or not (`top`), so the rank chooses only
-- among claims of that precedence. Then, as `claim_order` keys them: the rank, one
-- no marked link reached, the shorter path, the claimant's Node ID, and the key the
-- holder gives the neighbour that gave it. How Rust's chain maps onto the path,
-- and where it names fewer relays, is in this directory's `README.md`.
--------------------------------------------------------------------------------

module Claim where

open import Data.Bool.Base using (Bool; true; false; _∨_; _∧_; not; if_then_else_)
open import Data.List.Base using (List; []; length)
open import Data.Maybe.Base using (Maybe; nothing; just)
open import Data.Nat.Base using (ℕ; _+_; _≤ᵇ_; z≤n)
import Data.Nat.Properties as ℕ
open import Data.Product.Base using (_×_; _,_; proj₁)
open import Data.Sum.Base using (inj₁)
open import Relation.Binary.PropositionalEquality using (_≡_; refl; cong)
open import Relation.Nullary.Decidable.Core using (does)

open import RoutingLib.Routing.Basics.Path.Uncertified using (Path; Edge)

open import Order

------------------------------------------------------------------------
-- What leads the rank

-- How far below its own path the claim is shown, and its run.
Precedence : Set
Precedence = ℕ × Maybe ℕ

OC : Ordered Precedence
OC = lex nat runs

private
  none-least : ∀ r → Ordered._≤_ runs nothing r
  none-least nothing  = Ordered.refl runs {nothing}
  none-least (just _) = inj₁ (z≤n , λ ())

  c₀-least : ∀ c → Ordered._≤_ OC (0 , nothing) c
  c₀-least (a , r) = lex-≤ nat runs {0} {a} {nothing} {r} z≤n λ _ → none-least r

------------------------------------------------------------------------
-- What else a claim carries

-- A Node ID: relay, and 1 for a marked identity.
Who : Set
Who = ℕ × ℕ

-- The claimant, and the key its holder gives the neighbour that gave it.
Extra : Set
Extra = Who × ℕ

private
  OE : Ordered Extra
  OE = lex (lex nat nat) nat

  e₀ : Extra
  e₀ = (0 , 0) , 0

  e₀-least : ∀ e → Ordered._≤_ OE e₀ e
  e₀-least ((_ , _) , _) =
    lex-≤ (lex nat nat) nat (lex-≤ nat nat z≤n λ _ → z≤n) λ _ → z≤n

  -- Among equal ranks, marks, and lengths, by claimant and giver: one claimant has one
  -- claim of a broadcast, and one holder takes it from a giver once, so the path never
  -- decides between candidates at a node.
  OT : Ordered (Path × Extra)
  OT = keyed (λ (p , e) → e , p) (cong (λ (e , p) → p , e)) (lex OE (list (lex nat nat)))

  OT-least : ∀ e → Ordered._≤_ OT ([] , e₀) ([] , e)
  OT-least e = lex-≤ OE (list (lex nat nat)) (e₀-least e) λ _ → Ordered.refl (list (lex nat nat))

open import Increasing OC (0 , nothing) c₀-least e₀ OT OT-least public

------------------------------------------------------------------------
-- A step

-- A path that reaches this length at a giver is never adopted (`MAX_HOPS`).
max-chain : ℕ
max-chain = 32

record Step : Set where
  constructor step
  field
    up           : Bool
    cost         : ℕ
    penalty      : ℕ          -- 127 minus the link's or route's priority
    drains       : Bool       -- the giver sent GOAWAY, or the route is draining
    marked-here  : Bool       -- the receiver marked the link, or the route crossed a marked one
    marked-there : Bool       -- the giver marked the link
    adopts       : Bool       -- an adoption, not a domain step
    lowered      : ℕ          -- how far below the claim's path the receiver is shown it
    run          : Maybe ℕ    -- the publisher's run, on its own step
    top          : Precedence -- the best precedence the receiver knows of, reachable or not
    me           : ℕ          -- the receiver's relay
    giver        : ℕ          -- the receiver's key for the giver

private
  module S = Step

  first : Maybe ℕ → Maybe ℕ → Maybe ℕ
  first nothing  r = r
  first (just n) _ = just n

  rise : Step → Precedence → Precedence
  rise f (a , r) = a + S.lowered f , first r (S.run f)

  rise-≥ : ∀ f c → Ordered._≤_ OC c (rise f c)
  rise-≥ f (a , nothing) =
    lex-≤ nat runs {a} {a + S.lowered f} {nothing} {S.run f} (ℕ.m≤m+n a (S.lowered f)) λ _ → none-least (S.run f)
  rise-≥ f (a , just n)  =
    lex-≤ nat runs {a} {a + S.lowered f} {just n} {just n} (ℕ.m≤m+n a (S.lowered f)) λ _ → Ordered.refl runs {just n}

-- A step refuses a claim when it is down, when the claim crossed a marked link and
-- the giver marked this one, when an adoption would take a path at the longest
-- chain, or when the receiver knows a claim of better precedence.
refuses : Step → Edge → Valid → Bool
refuses f _ v =
  not (S.up f)
  ∨ (marked v ∧ S.marked-there f)
  ∨ (S.adopts f ∧ (max-chain ≤ᵇ length (path v)))
  ∨ not (does (Ordered._≤?_ OC (rise f (lead v)) (S.top f)))

-- An adoption files the claim under the receiver, marked once a marked link reached
-- it; a domain step keeps the announcer's claimant.
tag : Step → Edge → Valid → Extra
tag f _ v with S.adopts f
... | true  = (S.me f , (if marked v ∨ S.marked-here f then 1 else 0)) , S.giver f
... | false = proj₁ (extra v) , 0

policy : Policy
policy = record
  { Link         = Step
  ; down         = step false 0 0 false false false false 0 nothing (0 , nothing) 0 0
  ; rise         = rise
  ; rise-≥       = rise-≥
  ; cost         = S.cost
  ; penalty      = S.penalty
  ; drains       = S.drains
  ; marks        = λ _ _ _ → false
  ; marked       = S.marked-here
  ; refuses      = refuses
  ; down-refuses = λ _ _ → refl
  ; tag          = tag
  }

open Rule policy public
