--------------------------------------------------------------------------------
-- The algebra both levels of the route rule share, and the theorem it buys.
--
-- A route is invalid or carries whether it is draining, a penalty (the lowest
-- priority on its path, counted down from the highest), a cost, whether a marked
-- link is on its path, its path, and whatever else a level carries in `extra`.
-- Routes rank by draining, then penalty, then cost, then unmarked before marked,
-- then the shorter path, then a fixed order on paths, then `extra`. Crossing a
-- link (`extend`) refuses a route whose path does not start at the link's far end
-- or already names its near end, refuses whatever the link's own filter refuses,
-- and otherwise can only make the route draining, raise its penalty, add to its
-- cost, mark it, and prepend the link to its path.
--
-- Any such algebra is an increasing path algebra, so Daggitt and Griffin's
-- Theorem 5 applies: on every network, from every state, under every schedule
-- of delayed, reordered, lost, and repeated messages, the asynchronous iteration
-- reaches the network's unique fixed point once the network holds still long
-- enough (`convergent`).
--------------------------------------------------------------------------------

open import Data.Bool.Base using (Bool; true; false; _∨_; if_then_else_)
open import Data.List.Base using (List; []; _∷_)
open import Data.Nat.Base using (ℕ; zero; suc; _+_; _⊔_; z≤n; _^_)

open import RoutingLib.Routing.Basics.Path.Uncertified using (Path; Edge)

open import Order

module Increasing
  -- What else a route carries, ordered, with the trivial route's value least.
  {E : Set} (OE : Ordered E) (e₀ : E) (e₀-least : ∀ e → Ordered._≤_ OE e₀ e)
  -- How paths of equal length order.
  (OP : Ordered Path)
  where

import Algebra.Construct.NaturalChoice.Min as Min
import Data.Bool.Properties as Bool
import Data.Bool.Base as Boolᵇ
open import Data.Empty using (⊥-elim)
open import Data.Fin.Base using (toℕ)
import Data.Nat.Properties as ℕ
open import Data.Product.Base using (_×_; _,_; proj₁; proj₂)
open import Data.Sum.Base using (_⊎_; inj₁; inj₂)
open import Level using (0ℓ)
open import Relation.Binary.Bundles using (TotalOrder)
open import Relation.Binary.Core using (Rel)
open import Relation.Binary.Definitions using (Decidable; Minimum; Maximum)
open import Relation.Binary.PropositionalEquality
open import Relation.Nullary.Decidable.Core using (yes; no; map′)
open import Relation.Nullary.Negation.Core using (¬_; contradiction)

open import RoutingLib.Routing.Algebra
import RoutingLib.Routing.Basics.Path.UncertifiedI as PathI
open import RoutingLib.Routing.Basics.Path.Uncertified.Properties using (_⇿?_; _∈ₚ?_; p≉i∷p)
open import RoutingLib.Routing.VectorBased.Convergence.Results

------------------------------------------------------------------------
-- Routes

record Valid : Set where
  constructor route
  field
    draining : Bool
    penalty  : ℕ
    cost     : ℕ
    marked   : Bool
    path     : Path
    extra    : E

open Valid public

data Weight : Set where
  invalid : Weight
  valid   : Valid → Weight

-- The route a node has to itself.
trivial : Weight
trivial = valid (route false 0 0 false [] e₀)

------------------------------------------------------------------------
-- What crossing a link does

record Policy : Set₁ where
  field
    Link     : Set
    -- The link that is down.
    down     : Link
    -- What the link adds.
    cost     : Link → ℕ
    penalty  : Link → ℕ
    drains   : Link → Bool
    -- Whether the receiver ranks the route draining, from the link (near end,
    -- far end) and the path as offered.
    marks    : Link → Edge → Path → Bool
    -- Whether the link marks every route that crosses it.
    marked   : Link → Bool
    -- Whether the link refuses the route, from the link and the route as offered.
    refuses  : Link → Edge → Valid → Bool
    down-refuses : ∀ e v → refuses down e v ≡ true
    -- How the link changes the rest.
    tag      : Link → E → E

module Rule (P : Policy) where

  open Policy P using (Link; down; refuses; down-refuses; tag; marks; drains)
  private
    module P = Policy P

  -- The route as it stands across the link from i to j, j being the neighbour
  -- that offered it.
  extend : Link → Edge → Weight → Weight
  extend f e invalid = invalid
  extend f (i , j) (valid v) with (i , j) ⇿? path v | i ∈ₚ? path v
  ... | no  _ | _     = invalid
  ... | yes _ | yes _ = invalid
  ... | yes _ | no  _ = if refuses f (i , j) v then invalid else valid (route
    (draining v ∨ drains f ∨ marks f (i , j) (path v))
    (P.penalty f ⊔ penalty v)
    (P.cost f + cost v)
    (marked v ∨ P.marked f)
    ((i , j) ∷ path v)
    (tag f (extra v)))

  ------------------------------------------------------------------------
  -- The order

  private
    key : Valid → Bool × ℕ × ℕ × Bool × Path × E
    key v = draining v , penalty v , cost v , marked v , path v , extra v

    O₅ = lex (shortlex OP) OE
    O₄ = lex bool O₅
    O₃ = lex nat O₄
    O₂ = lex nat O₃

    OK : Ordered (Bool × ℕ × ℕ × Bool × Path × E)
    OK = lex bool O₂

  OV : Ordered Valid
  OV = keyed key (λ eq → cong (λ (d , q , c , m , p , x) → route d q c m p x) eq) OK

  private
    module OV = Ordered OV

  infix 4 _≤ᵣ_

  data _≤ᵣ_ : Rel Weight 0ℓ where
    below-invalid : ∀ {r} → r ≤ᵣ invalid
    below         : ∀ {v w} → v OV.≤ w → valid v ≤ᵣ valid w

  ≤ᵣ-total : ∀ r s → r ≤ᵣ s ⊎ s ≤ᵣ r
  ≤ᵣ-total r invalid = inj₁ below-invalid
  ≤ᵣ-total invalid (valid w) = inj₂ below-invalid
  ≤ᵣ-total (valid v) (valid w) with OV.total v w
  ... | inj₁ v≤w = inj₁ (below v≤w)
  ... | inj₂ w≤v = inj₂ (below w≤v)

  ≤ᵣ-refl : ∀ {r} → r ≤ᵣ r
  ≤ᵣ-refl {invalid} = below-invalid
  ≤ᵣ-refl {valid v} = below OV.refl

  ≤ᵣ-trans : ∀ {r s t} → r ≤ᵣ s → s ≤ᵣ t → r ≤ᵣ t
  ≤ᵣ-trans _ below-invalid = below-invalid
  ≤ᵣ-trans (below v≤w) (below w≤x) = below (OV.trans v≤w w≤x)

  ≤ᵣ-antisym : ∀ {r s} → r ≤ᵣ s → s ≤ᵣ r → r ≡ s
  ≤ᵣ-antisym below-invalid below-invalid = refl
  ≤ᵣ-antisym (below v≤w) (below w≤v) = cong valid (OV.antisym v≤w w≤v)

  ≤ᵣ-totalOrder : TotalOrder 0ℓ 0ℓ 0ℓ
  ≤ᵣ-totalOrder = record
    { Carrier = Weight
    ; _≈_ = _≡_
    ; _≤_ = _≤ᵣ_
    ; isTotalOrder = record
      { isPartialOrder = record
        { isPreorder = record
          { isEquivalence = isEquivalence
          ; reflexive = λ { refl → ≤ᵣ-refl }
          ; trans = ≤ᵣ-trans
          }
        ; antisym = ≤ᵣ-antisym
        }
      ; total = ≤ᵣ-total
      }
    }

  -- A node keeps the lower of two routes.
  open Min ≤ᵣ-totalOrder public using () renaming (_⊓_ to _⊕_)
  private
    module M = Min ≤ᵣ-totalOrder

  ------------------------------------------------------------------------
  -- Equality is decidable

  _≟ᵣ_ : Decidable {A = Weight} _≡_
  invalid ≟ᵣ invalid = yes refl
  invalid ≟ᵣ valid _ = no λ ()
  valid _ ≟ᵣ invalid = no λ ()
  valid v ≟ᵣ valid w = map′ (cong valid) (λ { refl → refl }) (v OV.≟ w)

  ------------------------------------------------------------------------
  -- The trivial route is the least, the invalid one the greatest

  private
    ≤-∨ : ∀ a b → a Boolᵇ.≤ (a ∨ b)
    ≤-∨ false b = Bool.≤-minimum b
    ≤-∨ true  b = Bool.≤-reflexive refl

    []-least : ∀ p → Ordered._≤_ (shortlex OP) [] p
    []-least []      = Ordered.refl (shortlex OP)
    []-least (e ∷ p) = inj₁ (z≤n , λ ())

  trivial-least : Minimum _≤ᵣ_ trivial
  trivial-least invalid = below-invalid
  trivial-least (valid w) = below
    (lex-≤ bool O₂ (Bool.≤-minimum (draining w)) λ _ →
     lex-≤ nat O₃ z≤n λ _ →
     lex-≤ nat O₄ z≤n λ _ →
     lex-≤ bool O₅ (Bool.≤-minimum (marked w)) λ _ →
     lex-≤ (shortlex OP) OE ([]-least (path w)) λ _ →
     e₀-least (extra w))

  invalid-greatest : Maximum _≤ᵣ_ invalid
  invalid-greatest _ = below-invalid

  ------------------------------------------------------------------------
  -- Crossing a link never improves a route

  extend-increasing : ∀ f e r → r ≤ᵣ extend f e r
  extend-increasing f e invalid = below-invalid
  extend-increasing f (i , j) (valid v) with (i , j) ⇿? path v | i ∈ₚ? path v
  ... | no  _ | _     = below-invalid
  ... | yes _ | yes _ = below-invalid
  ... | yes _ | no  _ with refuses f (i , j) v
  ...   | true  = below-invalid
  ...   | false = below
    (lex-≤ bool O₂ (≤-∨ (draining v) _) λ _ →
     lex-≤ nat O₃ (ℕ.m≤n⊔m (P.penalty f) (penalty v)) λ _ →
     lex-≤ nat O₄ (ℕ.m≤n+m (cost v) (P.cost f)) λ _ →
     lex-≤ bool O₅ (≤-∨ (marked v) _) λ _ →
     lex-≤ (shortlex OP) OE (shortlex-∷ OP (i , j) (path v)) λ eq →
     contradiction eq p≉i∷p)

  ------------------------------------------------------------------------
  -- The routing algebra

  algebra : RawRoutingAlgebra 0ℓ 0ℓ 0ℓ
  algebra = record
    { PathWeight         = Weight
    ; Step               = λ _ _ → Link
    ; _≈_                = _≡_
    ; _⊕_                = _⊕_
    ; _▷_                = λ {_} {i} {j} f r → extend f (toℕ i , toℕ j) r
    ; 0#                 = trivial
    ; ∞#                 = invalid
    ; f∞                 = λ _ _ → down
    ; ≈-isDecEquivalence = record { isEquivalence = isEquivalence ; _≟_ = _≟ᵣ_ }
    ; ⊕-cong             = cong₂ _⊕_
    ; ▷-cong             = λ _ → cong _
    ; f∞-reject          = λ i j → down-rejects (toℕ i , toℕ j)
    }
    where
    down-rejects : ∀ e r → extend down e r ≡ invalid
    down-rejects e invalid = refl
    down-rejects (i , j) (valid v) with (i , j) ⇿? path v | i ∈ₚ? path v
    ... | no  _ | _     = refl
    ... | yes _ | yes _ = refl
    ... | yes _ | no  _ rewrite down-refuses (i , j) v = refl

  isRoutingAlgebra : IsRoutingAlgebra algebra
  isRoutingAlgebra = record
    { ⊕-sel        = M.⊓-sel
    ; ⊕-comm       = M.⊓-comm
    ; ⊕-assoc      = M.⊓-assoc
    ; ⊕-zeroʳ      = M.⊓-zeroʳ trivial-least
    ; ⊕-identityʳ  = M.⊓-identityʳ invalid-greatest
    ; ▷-fixedPoint = λ _ → refl
    }

  isIncreasing : IsIncreasing algebra
  isIncreasing {i = i} {j} f r = sym (M.x≤y⇒y⊓x≈x (extend-increasing f (toℕ i , toℕ j) r))

  ------------------------------------------------------------------------
  -- The path algebra

  pathOf : Weight → PathI.Path
  pathOf invalid   = PathI.invalid
  pathOf (valid v) = PathI.valid (path v)

  private
    valid-injective : ∀ {p q : Path} → PathI.valid p ≡ PathI.valid q → p ≡ q
    valid-injective refl = refl

  path-reject : ∀ (i j : ℕ) f {r p} → pathOf r ≡ PathI.valid p →
                (¬ (i , j) PathI.⇿ᵥ p) ⊎ i PathI.∈ᵥₚ p → extend f (i , j) r ≡ invalid
  path-reject i j f {invalid} () _
  path-reject i j f {valid v} eq bad with (i , j) ⇿? path v | i ∈ₚ? path v
  ... | no  _   | _      = refl
  ... | yes _   | yes _  = refl
  ... | yes ij⇿ | no  i∉ with valid-injective eq | bad
  ...   | refl | inj₁ ¬ij⇿ = contradiction ij⇿ ¬ij⇿
  ...   | refl | inj₂ i∈   = contradiction i∈ i∉

  path-accept : ∀ (i j : ℕ) f {r p} → pathOf r ≡ PathI.valid p →
                extend f (i , j) r ≢ invalid → pathOf (extend f (i , j) r) ≡ PathI.valid ((i , j) ∷ p)
  path-accept i j f {invalid} () _
  path-accept i j f {valid v} eq ok with (i , j) ⇿? path v | i ∈ₚ? path v
  ... | no  _ | _     = contradiction refl ok
  ... | yes _ | yes _ = contradiction refl ok
  ... | yes _ | no  _ with refuses f (i , j) v
  ...   | true  = contradiction refl ok
  ...   | false with valid-injective eq
  ...     | refl = refl

  isPathAlgebra : IsPathAlgebra algebra
  isPathAlgebra = record
    { path           = pathOf
    ; path-cong      = cong pathOf
    ; r≈0⇒path[r]≈[] = λ { refl → refl }
    ; r≈∞⇒path[r]≈∅  = λ { refl → refl }
    ; path[r]≈∅⇒r≈∞  = λ { {invalid} _ → refl ; {valid _} () }
    ; path-reject    = λ {_} {i} {j} f → path-reject (toℕ i) (toℕ j) f
    ; path-accept    = λ {_} {i} {j} f → path-accept (toℕ i) (toℕ j) f
    }

  ------------------------------------------------------------------------
  -- The theorem

  -- Daggitt and Griffin, Theorem 5: on every network, from every state, under
  -- every schedule, the asynchronous iteration reaches the unique fixed point of
  -- each epoch that lasts long enough.
  convergent : Convergent algebra
  convergent = incrPaths⇒convergent isRoutingAlgebra isPathAlgebra isIncreasing

  -- And the synchronous iteration settles within n² rounds on n nodes.
  converges-n² : SynchronouslyConvergesIn algebra (λ n → n ^ 2)
  converges-n² = incrPaths⇒syncConvergesIn-n² isRoutingAlgebra isPathAlgebra isIncreasing
