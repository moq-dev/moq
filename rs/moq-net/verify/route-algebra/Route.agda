--------------------------------------------------------------------------------
-- Routes between forwarding nodes, as `route::relay::Router` ranks and extends
-- them (`rs/moq-net/src/route/relay.rs`).
--
-- A node is a Node ID: one run of one relay, a restarted relay being a new relay,
-- and its marked identity, which announces what the node took over a marked link.
-- `relay` and `mark` read the two back from a node: the floor files a closed
-- session by relays, and equal routes order by the neighbour's Node ID. A path
-- names a marked identity only right before its node, so one naming the
-- receiver's relay names the receiver, which the algebra refuses.
--
-- A node's marked identity is a node whose one link is from the node itself: up,
-- free, marked, at session 0, so its route there is the node's marked route.
--
-- A route at node i is what i would offer: its path names i first, then i's next
-- hop, and so on to the forwarding node it leads to. `extra` carries, beside each
-- link, the session the nearer node holds it on. The link from i to j carries what i and j hold about it and what i
-- knows: closed sessions (floors) and nodes named draining.
--------------------------------------------------------------------------------

open import Data.Bool.Base using (Bool; true; false; _∨_; _∧_; not; if_then_else_)
open import Data.List.Base using (List; []; _∷_; map; any; zip; null; length)
open import Data.Nat.Base using (ℕ; suc; _+_; _≡ᵇ_; _<ᵇ_; _≤ᵇ_)
open import Data.Product.Base using (_×_; _,_; proj₁; proj₂)
open import Relation.Binary.PropositionalEquality using (_≡_; refl; cong)

open import RoutingLib.Routing.Basics.Path.Uncertified using (Path; Edge)

open import Order

module Route (relay mark : ℕ → ℕ) where

open import Data.List.Properties using (map-injective)
open import Data.List.Relation.Binary.Lex.Core using (base; halt)
open import Data.Product.Properties using (,-injectiveˡ; ,-injectiveʳ)
open import Data.Unit.Base using (⊤; tt)
import Data.Unit.Properties as Unit

-- The longest path a node takes, counting the node it leads to (`MAX_HOPS`).
max-hops : ℕ
max-hops = 32

------------------------------------------------------------------------
-- What a route carries beyond its rank and path

-- The session beside each link.
Extra : Set
Extra = List ℕ

OE : Ordered Extra
OE = list nat

e₀ : Extra
e₀ = []

e₀-least : ∀ e → Ordered._≤_ OE e₀ e
e₀-least []      = base _
e₀-least (_ ∷ _) = halt

-- Among equal ranks and lengths, by the neighbour's Node ID: its relay, then
-- unmarked first, as `NodeId` orders. No two candidates at one node share a
-- neighbour, so the rest of the path never decides between them.
OP : Ordered Path
OP = keyed (map neighbour) (map-injective neighbour-injective) (list (lex nat (lex nat (lex nat nat))))
  where
  neighbour : Edge → ℕ × ℕ × ℕ × ℕ
  neighbour (a , b) = a , relay b , mark b , b

  neighbour-injective : ∀ {x y} → neighbour x ≡ neighbour y → x ≡ y
  neighbour-injective {a , b} {c , d} eq with ,-injectiveˡ eq
  ... | refl = cong (a ,_) (,-injectiveʳ (,-injectiveʳ (,-injectiveʳ eq)))

-- Nothing leads a route's rank.
OC : Ordered ⊤
OC = record { _≤_ = _≡_ ; isDecTotalOrder = Unit.≡-isDecTotalOrder }

OT : Ordered (Path × Extra)
OT = lex OP OE

open import Increasing OC tt (λ _ → refl) e₀ OT (λ e → lex-≤ OP OE (Ordered.refl OP) λ _ → e₀-least e) public

------------------------------------------------------------------------
-- The link

-- The highest session known closed from relay `near` to relay `far`, as near
-- numbered it.
record Floor : Set where
  constructor floor
  field
    near far session : ℕ

-- The link from the receiver to the neighbour that offers it a route.
record Link : Set where
  constructor link
  field
    up           : Bool       -- false for a link that is down
    cost         : ℕ          -- the receiver's port cost, at least 1 in Rust
    penalty      : ℕ          -- 127 minus the receiver's port priority
    drains       : Bool       -- the neighbour sent GOAWAY
    draining     : List ℕ     -- nodes the receiver knows draining
    floors       : List Floor -- the receiver's closed sessions
    marked-here  : Bool       -- the receiver marked the link
    marked-there : Bool       -- the neighbour marked the link
    claimed      : Bool       -- the neighbour announces a claim
    session      : ℕ          -- the receiver's session number on the link

private
  module L = Link

_∈ᵇ_ : ℕ → List ℕ → Bool
n ∈ᵇ ns = any (n ≡ᵇ_) ns

-- Whether `floors` hold the session from `a` to `b` numbered `s` closed.
closed : List Floor → Edge × ℕ → Bool
closed fs ((a , b) , s) = any at-or-below fs
  where
  at-or-below : Floor → Bool
  at-or-below (floor n f se) = (relay a ≡ᵇ n) ∧ (relay b ≡ᵇ f) ∧ (s ≤ᵇ se)

-- The largest cost a route carries (`Metric`).
metric : ℕ
metric = 18446744073709551615

-- A link refuses a route when it is down, when the route is the neighbour's
-- route to itself and the neighbour announces no claim, when the route crossed a
-- marked link and this one is marked at the neighbour, when the path would grow
-- past `max-hops`, when the cost would pass the metric, or when the path names a
-- session the receiver knows closed.
refuses : Link → Edge → Valid → Bool
refuses f _ v =
  not (L.up f)
  ∨ (null (path v) ∧ not (L.claimed f))
  ∨ (marked v ∧ L.marked-there f)
  ∨ (max-hops ≤ᵇ suc (length (path v)))
  ∨ (metric <ᵇ L.cost f + cost v)
  ∨ any (closed (L.floors f)) (zip (path v) (extra v))

-- The receiver ranks a route draining when it names a node known draining.
marks : Link → Edge → Path → Bool
marks f (_ , j) p = any (_∈ᵇ L.draining f) (j ∷ map proj₂ p)

policy : Policy
policy = record
  { Link         = Link
  ; down         = link false 0 0 false [] [] false false false 0
  ; rise         = λ _ c → c
  ; rise-≥       = λ _ _ → refl
  ; cost         = L.cost
  ; penalty      = L.penalty
  ; drains       = L.drains
  ; marks        = marks
  ; marked       = L.marked-here
  ; refuses      = refuses
  ; down-refuses = λ _ _ → refl
  ; tag          = λ f _ v → L.session f ∷ extra v
  }

open Rule policy public
