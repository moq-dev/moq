--------------------------------------------------------------------------------
-- Decidable total orders whose equality is _≡_, and the two ways the route
-- algebra builds one from smaller ones: lexicographically, and through an
-- injective key.
--------------------------------------------------------------------------------

module Order where

import Data.Bool.Base as Boolᵇ
open Boolᵇ using (Bool)
import Data.Bool.Properties as Bool
open import Data.List.Base using (List)
open import Data.Maybe.Base using (Maybe; nothing; just)
import Data.List.Relation.Binary.Lex.NonStrict as ListLex
open import Data.List.Relation.Binary.Pointwise using (Pointwise-≡⇒≡; ≡⇒Pointwise-≡)
import Data.Nat.Base as ℕᵇ
open ℕᵇ using (ℕ)
import Data.Nat.Properties as ℕ
open import Data.Product.Base using (_×_; _,_; proj₁; proj₂)
import Data.Product.Relation.Binary.Lex.NonStrict as ProductLex
open import Data.Product.Relation.Binary.Pointwise.NonDependent using (≡×≡⇒≡; ≡⇒≡×≡)
open import Data.Sum.Base using (inj₁; inj₂)
open import Function.Base using (flip)
open import Level using (0ℓ)
open import Relation.Binary.Core using (Rel; _⇒_)
open import Relation.Binary.Definitions using (Decidable)
open import Relation.Binary.PropositionalEquality.Core using (_≡_; _≢_; refl; cong)
import Relation.Binary.PropositionalEquality.Properties as ≡
open import Relation.Binary.Structures using (IsDecTotalOrder)
import Relation.Binary.Construct.Flip.EqAndOrd as Flip
open import Relation.Nullary.Decidable.Core using (yes; no; map′)

-- A decidable total order on A whose equality is _≡_.
record Ordered (A : Set) : Set₁ where
  field
    _≤_             : Rel A 0ℓ
    isDecTotalOrder : IsDecTotalOrder _≡_ _≤_

  open IsDecTotalOrder isDecTotalOrder public

-- An order whose equality is pointwise _≡_ is an order over _≡_.
via≡ : ∀ {A : Set} {_≈_ _≤_ : Rel A 0ℓ} →
       _≈_ ⇒ _≡_ → _≡_ ⇒ _≈_ → IsDecTotalOrder _≈_ _≤_ → Ordered A
via≡ {_≈_ = _≈_} {_≤_} to from o = record
  { _≤_             = _≤_
  ; isDecTotalOrder = record
    { isTotalOrder = record
      { isPartialOrder = record
        { isPreorder = record
          { isEquivalence = ≡.isEquivalence
          ; reflexive     = λ eq → O.reflexive (from eq)
          ; trans         = O.trans
          }
        ; antisym = λ x≤y y≤x → to (O.antisym x≤y y≤x)
        }
      ; total = O.total
      }
    ; _≟_  = λ x y → map′ to from (x O.≟ y)
    ; _≤?_ = O._≤?_
    }
  }
  where module O = IsDecTotalOrder o

bool : Ordered Bool
bool = record { _≤_ = Boolᵇ._≤_ ; isDecTotalOrder = Bool.≤-isDecTotalOrder }

nat : Ordered ℕ
nat = record { _≤_ = ℕᵇ._≤_ ; isDecTotalOrder = ℕ.≤-isDecTotalOrder }

-- Compare the first components, and the second only between equal firsts.
lex : ∀ {A B : Set} → Ordered A → Ordered B → Ordered (A × B)
lex OA OB = via≡ ≡×≡⇒≡ ≡⇒≡×≡ (ProductLex.×-isDecTotalOrder OA.isDecTotalOrder OB.isDecTotalOrder)
  where module OA = Ordered OA; module OB = Ordered OB

-- Lexicographic, a prefix first.
list : ∀ {A : Set} → Ordered A → Ordered (List A)
list OA = via≡ Pointwise-≡⇒≡ ≡⇒Pointwise-≡ (ListLex.≤-isDecTotalOrder OA.isDecTotalOrder)
  where module OA = Ordered OA

-- Order A by an injective key into B.
keyed : ∀ {A B : Set} (f : A → B) → (∀ {x y} → f x ≡ f y → x ≡ y) → Ordered B → Ordered A
keyed f inj OB = record
  { _≤_             = λ x y → f x OB.≤ f y
  ; isDecTotalOrder = record
    { isTotalOrder = record
      { isPartialOrder = record
        { isPreorder = record
          { isEquivalence = ≡.isEquivalence
          ; reflexive     = λ { refl → OB.refl }
          ; trans         = OB.trans
          }
        ; antisym = λ x≤y y≤x → inj (OB.antisym x≤y y≤x)
        }
      ; total = λ x y → OB.total (f x) (f y)
      }
    ; _≟_  = λ x y → map′ inj (cong f) (f x OB.≟ f y)
    ; _≤?_ = λ x y → f x OB.≤? f y
    }
  }
  where module OB = Ordered OB

-- Higher first.
newest : Ordered ℕ
newest = record { _≤_ = flip ℕᵇ._≤_ ; isDecTotalOrder = Flip.isDecTotalOrder ℕ.≤-isDecTotalOrder }

-- None first, then the higher.
runs : Ordered (Maybe ℕ)
runs = keyed key key-injective (lex nat newest)
  where
  key : Maybe ℕ → ℕ × ℕ
  key nothing  = 0 , 0
  key (just n) = 1 , n

  key-injective : ∀ {x y} → key x ≡ key y → x ≡ y
  key-injective {nothing} {nothing} _    = refl
  key-injective {just _}  {just _}  refl = refl

------------------------------------------------------------------------
-- Building a lexicographic comparison one component at a time

module _ {A B : Set} (OA : Ordered A) (OB : Ordered B) where
  private
    module OA = Ordered OA
    module OB = Ordered OB
    module OL = Ordered (lex OA OB)

  -- First components in order, and the second ones too when the first are equal.
  lex-≤ : ∀ {a a' b b'} → a OA.≤ a' → (a ≡ a' → b OB.≤ b') → (a , b) OL.≤ (a' , b')
  lex-≤ {a} {a'} a≤a' k with a OA.≟ a'
  ... | yes eq = inj₂ (eq , k eq)
  ... | no  ne = inj₁ (a≤a' , ne)

