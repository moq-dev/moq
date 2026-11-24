--------------------------------------------------------------------------------
-- The floor and the named drains only make the rule arrive sooner.
--
-- Both are what a node learns about links other than its own: sessions known
-- closed, and nodes named draining. Without them (`plain`) the rule is the same
-- algebra over the same network, so it converges too. Every route at a fixed
-- point of `plain` crosses only live links, each beside the session it is live
-- on, and is draining wherever a link on it carries a GOAWAY (`sound`). So a floor
-- that holds only closed sessions refuses none of those routes, and naming a node
-- draining once it sent GOAWAY on every live link marks none of them draining that
-- was not. The fixed point of `plain` is then a fixed point with both
-- (`accelerated-fixed`): neither removes or demotes a route the iteration itself
-- would keep.
--------------------------------------------------------------------------------

open import Data.Nat.Base using (ℕ)
open import Relation.Binary.PropositionalEquality using (_≡_)

module Accelerate (relay mark : ℕ → ℕ) where

open import Data.Bool.Base using (Bool; true; false; _∨_; _∧_; not)
import Data.Bool.Properties as Bool
open import Data.Fin.Base using (Fin; toℕ)
open import Data.List.Base using (List; []; _∷_; map; any; zip; length; foldr; tabulate; null)
open import Data.List.Properties using (tabulate-cong)
open import Data.List.Relation.Unary.All using (All; []; _∷_)
import Data.List.Relation.Unary.All as All
open import Data.Nat.Base using (zero; suc; _+_; _<_; _≤ᵇ_; _<ᵇ_)
import Data.Nat.Properties as ℕ
open import Data.Product.Base using (_×_; _,_; ∃₂; proj₁; proj₂)
open import Data.Sum.Base using (_⊎_; inj₁; inj₂)
open import Data.Unit.Base using (⊤; tt)
open import Relation.Binary.PropositionalEquality using (refl; sym; trans; cong; subst)
open import Relation.Nullary.Decidable.Core using (yes; no)

open import RoutingLib.Routing.Basics.Path.Uncertified using (Path; Edge)
open import RoutingLib.Routing.Basics.Path.Uncertified.Properties using (_⇿?_; _∈ₚ?_)
open import RoutingLib.Routing.Basics.Network using (AdjacencyMatrix)

open import Route relay mark

private
  -- A link refuses every route while it is down.
  refuses-up : ∀ f e x → refuses f e x ≡ false → Link.up f ≡ true
  refuses-up (link true _ _ _ _ _ _ _ _ _) e x _ = refl
  refuses-up (link false _ _ _ _ _ _ _ _ _) e x ()

  -- No floor, no refusal; no one named draining, no one marked.
  no-floor : ∀ zs → any (closed []) zs ≡ false
  no-floor []       = refl
  no-floor (_ ∷ zs) = no-floor zs

  no-drain : ∀ ns → any (_∈ᵇ []) ns ≡ false
  no-drain []       = refl
  no-drain (_ ∷ ns) = no-drain ns

  ∨-true : ∀ a {b} → b ≡ true → a ∨ b ≡ true
  ∨-true false refl = refl
  ∨-true true  _    = refl

  absorb : ∀ a b → a ∨ b ≡ true → a ∨ (b ∨ true) ≡ a ∨ (b ∨ false)
  absorb true  _     _  = refl
  absorb false true  _  = refl
  absorb false false ()

module _ {n : ℕ} (A : AdjacencyMatrix algebra n) where

  -- A's links, with no floors and no node named draining.
  plain : AdjacencyMatrix algebra n
  plain i k = record (A i k) { floors = [] ; draining = [] }

  open import RoutingLib.Routing.VectorBased.Synchronous algebra A using (F; RoutingMatrix; I; Iᵢⱼ≈0⊎∞)
  open import RoutingLib.Routing.VectorBased.Synchronous algebra plain using () renaming (F to F₀)
  open import RoutingLib.Routing.VectorBased.Synchronous.DistanceVector.Properties isRoutingAlgebra plain
    using (FXᵢⱼ≈Aᵢₖ▷Xₖⱼ⊎Iᵢⱼ)

  ------------------------------------------------------------------------
  -- What the two must respect

  -- A floor holds no session live on a link.
  FloorsClosed : Set
  FloorsClosed = ∀ i k (u v : Fin n) → Link.up (A u v) ≡ true →
                 closed (Link.floors (A i k)) ((toℕ u , toℕ v) , Link.session (A u v)) ≡ false

  -- A node is named draining only once it sent GOAWAY on every live link.
  DrainNamed : Set
  DrainNamed = ∀ i k (u v : Fin n) → toℕ v ∈ᵇ Link.draining (A i k) ≡ true →
               Link.up (A u v) ≡ true → Link.drains (A u v) ≡ true

  ------------------------------------------------------------------------
  -- Routes that name only live links

  -- A link on a route: live, beside the session it is live on, and the route is
  -- draining (`d`) if the link carries a GOAWAY.
  Holds : Bool → Edge × ℕ → Set
  Holds d ((u , v) , s) = ∃₂ λ (u′ v′ : Fin n) →
    toℕ u′ ≡ u × toℕ v′ ≡ v × Link.up (A u′ v′) ≡ true × Link.session (A u′ v′) ≡ s ×
    (Link.drains (A u′ v′) ≡ true → d ≡ true)

  Sound : Weight → Set
  Sound invalid   = ⊤
  Sound (valid x) = length (extra x) ≡ length (path x) ×
                    All (Holds (draining x)) (zip (path x) (extra x))

  private
    weaken : ∀ {d d′} → (d ≡ true → d′ ≡ true) → ∀ {z} → Holds d z → Holds d′ z
    weaken f (u′ , v′ , eu , ev , up , s , dr) = u′ , v′ , eu , ev , up , s , λ x → f (dr x)

    size : Weight → ℕ
    size invalid   = 0
    size (valid x) = length (path x)

  -- A step of `plain` keeps a route sound, where it needs the route it extends to be.
  extend-sound : ∀ (k m : Fin n) w → (∀ {y} → extend (plain k m) (toℕ k , toℕ m) w ≡ valid y → Sound w) →
                 Sound (extend (plain k m) (toℕ k , toℕ m) w)
  extend-sound k m invalid _ = tt
  extend-sound k m (valid x) need
    with (toℕ k , toℕ m) ⇿? path x | toℕ k ∈ₚ? path x
  ... | no  _ | _     = tt
  ... | yes _ | yes _ = tt
  ... | yes _ | no  _ with refuses (plain k m) (toℕ k , toℕ m) x in eq
  ...   | true  = tt
  ...   | false with need refl
  ...     | len , holds = cong suc len ,
    (k , m , refl , refl , refuses-up (plain k m) (toℕ k , toℕ m) x eq , refl ,
       λ dr → ∨-true (draining x) (subst (λ b → b ∨ _ ≡ true) (sym dr) refl)) ∷
    All.map (weaken λ d → subst (λ b → b ∨ _ ≡ true) (sym d) refl) holds

  -- A step lengthens the path it takes.
  extend-size : ∀ f e w {y} → extend f e w ≡ valid y → size w < length (path y)
  extend-size f e invalid ()
  extend-size f (i , j) (valid x) with (i , j) ⇿? path x | i ∈ₚ? path x
  ... | no  _ | _     = λ ()
  ... | yes _ | yes _ = λ ()
  ... | yes _ | no  _ with refuses f (i , j) x
  ...   | true  = λ ()
  ...   | false = λ { refl → ℕ.n<1+n _ }

  -- Every route at a fixed point of `plain` is sound.
  module _ (X : RoutingMatrix) (fixed : ∀ i j → F₀ X i j ≡ X i j) where

    private
      sound′ : ∀ fuel k j → size (X k j) < fuel → Sound (X k j)
      sound′ (suc fuel) k j lt with FXᵢⱼ≈Aᵢₖ▷Xₖⱼ⊎Iᵢⱼ X k j
      ... | inj₂ FX≡I = subst Sound (trans (sym FX≡I) (fixed k j)) (identity (Iᵢⱼ≈0⊎∞ k j))
        where
        identity : ∀ {r} → (r ≡ trivial) ⊎ (r ≡ invalid) → Sound r
        identity (inj₁ refl) = refl , []
        identity (inj₂ refl) = tt
      ... | inj₁ (m , FX≡) = subst Sound (trans (sym FX≡) (fixed k j)) (extend-sound k m (X m j) need)
        where
        need : ∀ {y} → extend (plain k m) (toℕ k , toℕ m) (X m j) ≡ valid y → Sound (X m j)
        need eq = sound′ fuel m j (ℕ.<-≤-trans (extend-size _ _ (X m j) eq)
          (ℕ.≤-pred (subst (λ r → size r < suc fuel) (trans (sym (fixed k j)) (trans FX≡ eq)) lt)))

    sound : ∀ k j → Sound (X k j)
    sound k j = sound′ (suc (size (X k j))) k j (ℕ.n<1+n _)

  ------------------------------------------------------------------------
  -- The floor and the named drains change no sound route

  module _ (floors-closed : FloorsClosed) (drain-named : DrainNamed) where

    private
      floors-silent : ∀ i k {d} zs → All (Holds d) zs → any (closed (Link.floors (A i k))) zs ≡ false
      floors-silent i k []       []       = refl
      floors-silent i k (_ ∷ zs) ((u′ , v′ , refl , refl , up , refl , _) ∷ hs)
        rewrite floors-closed i k u′ v′ up = floors-silent i k zs hs

      named-drained : ∀ i k {d} p ss → length ss ≡ length p → All (Holds d) (zip p ss) →
                      any (_∈ᵇ Link.draining (A i k)) (map proj₂ p) ≡ true → d ≡ true
      named-drained i k [] _ _ _ ()
      named-drained i k (_ ∷ _) [] () _ _
      named-drained i k (_ ∷ p) (_ ∷ ss) len ((u′ , v′ , refl , refl , up , refl , dr) ∷ hs) named
        with toℕ v′ ∈ᵇ Link.draining (A i k) in vin
      ... | true  = dr (drain-named i k u′ v′ vin up)
      ... | false = named-drained i k p ss (ℕ.suc-injective len) hs named

    -- A link of A refuses a sound route exactly when `plain` does.
    same-refusal : ∀ i k x → All (Holds (draining x)) (zip (path x) (extra x)) →
                   refuses (A i k) (toℕ i , toℕ k) x ≡ refuses (plain i k) (toℕ i , toℕ k) x
    same-refusal i k x holds = trans (cong prefix (floors-silent i k _ holds)) (sym (cong prefix (no-floor (zip (path x) (extra x)))))
      where
      prefix : Bool → Bool
      prefix b = not (Link.up (A i k)) ∨ (null (path x) ∧ not (Link.claimed (A i k)))
        ∨ (marked x ∧ Link.marked-there (A i k)) ∨ (max-hops ≤ᵇ suc (length (path x)))
        ∨ (metric <ᵇ Link.cost (A i k) + cost x) ∨ b

    -- One step across a link of A takes a sound route as `plain` does.
    neutral : ∀ i k w → Sound w → extend (A i k) (toℕ i , toℕ k) w ≡ extend (plain i k) (toℕ i , toℕ k) w
    neutral i k invalid _ = refl
    neutral i k (valid x) (len , holds)
      with (toℕ i , toℕ k) ⇿? path x | toℕ i ∈ₚ? path x
    ... | no  _ | _     = refl
    ... | yes _ | yes _ = refl
    ... | yes _ | no  _ rewrite same-refusal i k x holds with refuses (plain i k) (toℕ i , toℕ k) x in eq
    ...   | true  = refl
    ...   | false rewrite no-drain (map proj₂ (path x)) = cong (λ d → valid (route _ d _ _ _ _ _)) drained
      where
      up = refuses-up (plain i k) (toℕ i , toℕ k) x eq
      -- What the drains add, given the route is already draining where they would.
      drained : draining x ∨ (Link.drains (A i k) ∨ marks (A i k) (toℕ i , toℕ k) (path x))
              ≡ draining x ∨ (Link.drains (A i k) ∨ false)
      drained with marks (A i k) (toℕ i , toℕ k) (path x) in m
      ... | false = refl
      ... | true  = absorb (draining x) (Link.drains (A i k)) (named m)
        where
        named : marks (A i k) (toℕ i , toℕ k) (path x) ≡ true → draining x ∨ Link.drains (A i k) ≡ true
        named m′ with toℕ k ∈ᵇ Link.draining (A i k) in kin
        ... | true  = ∨-true (draining x) (drain-named i k i k kin up)
        ... | false = subst (λ b → b ∨ _ ≡ true) (sym (named-drained i k (path x) _ len holds m′)) refl

    -- The fixed point of `plain` is a fixed point of A.
    accelerated-fixed : (X : RoutingMatrix) → (∀ i j → F₀ X i j ≡ X i j) → ∀ i j → F X i j ≡ X i j
    accelerated-fixed X fixed i j = trans
      (cong (foldr _⊕_ (I i j)) (tabulate-cong λ k → neutral i k (X k j) (sound X fixed k j)))
      (fixed i j)
