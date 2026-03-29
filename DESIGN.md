# Type-Driven Design in Rust

## 0. Reading Rule

Read for invariants, not syntax. Examples illustrate the rule; they are not templates to copy mechanically.

This contract is stricter than raw Rust semantics. If Rust permits a construct that this document forbids, this document wins. Enforce the contract mechanically with CI linting, architectural tests, and `#![deny(unsafe_code)]` at the crate level.

If core logic requires `.unwrap()`, `.expect()`, `.clone()`, `unsafe`, `Arc<Mutex<T>>`, or `Rc<RefCell<T>>` to make the model workable, the invariant or ownership model is wrong.

## I. Core Contract

Invalid states MUST be unrepresentable. Types MUST make invalid states impossible to construct or impossible to pass across internal boundaries. Core logic MUST NOT rely on discipline, runtime checks, comments, or documentation to preserve invariants.

If an invalid domain state can exist after `cargo check`, the type design is wrong. `cargo check` MUST be evidence of structural integrity, not mere syntax validity.

## II. Definitions

For this document:

- **Repo-defined surface** means any type or signature declared in this repository, including struct fields, enum payloads, type aliases, config DTOs, persisted-state models, function parameters, and return types.
- **Boundary** means the immediate deserialization layer, FFI adapter, or external API handler that first receives foreign data.
- **Core logic** means internal business/domain logic after boundary translation.
- **Strict-surface API** means an API outside explicitly named runtime-shell or compatibility-quarantine modules.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as defined by BCP 14 (RFC 2119 and RFC 8174) when, and only when, shown in all caps.

## III. Absolute Modeling Rules

### A. No Optionality in Repo-Defined Types

Repo-defined surfaces MUST NOT contain `Option<T>`, including nested forms such as `Vec<Option<T>>` or `HashMap<K, Option<V>>`.

Repo-defined types MUST NOT replace `Option<T>` with pseudo-optional wrappers such as `Absent | Present(T)`, `Missing | Value(T)`, `Unknown | Known(T)`, or any equivalent variant pair where one branch means only "no value".

If data can be absent, the model is wrong. Fix it by one of the following:

1. split the model into distinct inhabited types,
2. use typestate, or
3. introduce a genuine domain mode whose variants name behavior, not absence.

Mere absence MUST NOT be modeled as a variant.

Use explicit operational modes instead of presence wrappers:

```rust
pub struct FeatureConfig;

pub enum FeatureMode {
    OfflineOnly,
    ProviderBacked(FeatureConfig),
}
```

`OfflineOnly` is allowed because it names behavior. `Absent` is forbidden because it names missing data.

### B. No Bare Booleans

Repo-defined surfaces MUST NOT contain `bool`, including nested forms such as `Vec<bool>` or `HashMap<K, bool>`.

Every boolean distinction stored or passed MUST be named with a two-variant enum, typestate, capability token, or split inhabited types. `true` and `false` are storage encodings, not domain language.

### C. No Wildcard Bypass Variants

Repo-defined config and permission ADTs MUST NOT define wildcard variants such as `Any`, `All`, `Unrestricted`, or `AllowAll`.

Use finite policy lattices with explicit merge functions instead.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum ApprovalMode {
    Strict,
    Balanced,
    Permissive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ApprovalModeCeiling {
    Strict,
    Balanced,
    Permissive,
}

fn enforce_ceiling(requested: ApprovalMode, ceiling: ApprovalModeCeiling) -> ApprovalMode {
    match (requested, ceiling) {
        (_, ApprovalModeCeiling::Strict) => ApprovalMode::Strict,
        (ApprovalMode::Permissive, ApprovalModeCeiling::Balanced) => ApprovalMode::Balanced,
        (mode, ApprovalModeCeiling::Permissive) => mode,
        (mode, ApprovalModeCeiling::Balanced) => mode,
    }
}
```

Boundary absence is permitted only transiently, but it MUST collapse into an inhabited requirement value before any repo-defined value is constructed.

### D. `Result` Is Control Flow, Not Domain State

Returning `Result<T, E>` from fallible operations is permitted. It MUST NOT be stored in domain state or other repo-defined data types.

`Result<T, ()>` and `Result<T, NotFound>` MUST NOT be used as backdoors for optionality.

### E. External Primitives Are Boundary-Local Only

External `Option` and `bool` MUST NOT be stored, forwarded, or bound to named locals in core logic. Collapse them immediately in the exact boundary function that receives them, either:

- inline in a `match`, or
- inside `TryFrom`, `Deserialize`, or equivalent boundary conversion code.

Do not assign an external `Option` to a `let` and juggle it across later lines. Do not forward raw optionality or booleans through helper layers.

## IV. Naming Rules

Repo-defined enums MUST use consequence-first names. A variant name MUST describe either:

- the downstream operational consequence, or
- the definitive domain fact established by that variant.

Repo-defined enums MUST NOT use generic lifecycle or container labels such as `Ready`, `Unavailable`, `Present`, `Missing`, `Connected`, `Disconnected`, `Reported`, or `NotReported`.

Variant names MUST NOT describe the upstream process that produced the state. They MUST describe what the system now knows or what the system must now do.

If a downstream consumer must inspect the constructor or origin function to understand how to behave, the name has failed. The compiler enforces structure; naming MUST enforce semantics. When designing enums, typestates, or structural boundaries, the name MUST dictate how the application behaves when encountering that state.

When a variant encodes a restriction, fallback, or denial, the name MUST describe the exact resulting perimeter or behavior. Use `HostIsolated` rather than `Sandboxed`; use `FollowPlatformDefault` rather than `Absent`.

## V. Typestate, ADTs, and State as Location

### A. Typestate

Use typestate to model phase transitions. Distinct phases MUST be distinct types to the compiler. Transitions MUST consume the prior state and return the next state.

```rust
pub struct ShuffledDeck {
    top: Card,
    rest: Vec<Card>,
    _proof: (),
}

pub enum DrawOutcome {
    CardDrawn { card: Card, remaining: ShuffledDeck },
    FinalCardDrawn { card: Card },
}

impl ShuffledDeck {
    pub fn new() -> Self { /* constructs a valid shuffled deck */ }

    #[must_use]
    pub fn draw(self) -> DrawOutcome { /* consumes deck */ }
}

pub fn start_game(deck: ShuffledDeck) { /* ... */ }
```

`ShuffledDeck` exists; an unshuffled game deck does not. `draw(self)` consumes the deck. The internal `Option` from `Vec::pop` stays boundary-local inside the method; it does not appear in any repo-defined type.

Typestate structs and capability tokens MUST NOT derive `Copy` or `Clone`. Duplication defeats move semantics and reopens states that were supposed to be consumed.

### B. Parametric Typestate

When multiple states share one data layout, a sealed marker parameter MUST be used.

```rust
mod state {
    pub trait ConfigPhase: private::Sealed {}
    mod private { pub trait Sealed {} }

    pub struct Draft;
    pub struct Validated;

    impl private::Sealed for Draft {}
    impl private::Sealed for Validated {}
    impl ConfigPhase for Draft {}
    impl ConfigPhase for Validated {}
}

pub struct AppConfig<S: state::ConfigPhase> {
    id: NonEmptyString,
    payload: NonEmptyString,
    _phase: std::marker::PhantomData<S>,
}
```

When states share layout, this pattern MUST be used. When states require different fields, separate structs MUST be used. Do not reintroduce `Option` to fake a shared layout.

### C. Use Sum Types for Exclusive States

Product types multiply state space. Sum types add it. Multiplication MUST NOT be used when the domain requires exclusive alternatives.

Invalid:

```rust
struct Connection {
    state: ConnectionState,
    socket: Option<TcpStream>,
}
```

Valid:

```rust
pub enum TransportSession {
    NetworkUnavailable,
    EstablishedStream(TcpStream),
}
```

### D. State as Location

Collections MUST NOT carry lifecycle state through status flags. Location is the state.

Use distinct locations such as `pending_uploads: Vec<PendingAsset>` and `resident_textures: Vec<ResidentAsset>`. Transitions MUST consume from one location and produce into another. Presence in the destination location is the proof.

### E. Iterator Style

Iterator combinators MUST be used instead of imperative `for` loops with mutable accumulation. Combinators make transformation stages explicit and keep the `Option` from `Iterator::next()` inside iterator machinery.

## VI. Parametricity and Trait Bounds

Use unconstrained generics when a function MUST remain blind to the element type.

```rust
fn get_first<T>(list: &NonEmptyList<T>) -> &T {
    list.first()
}
```

With no trait bounds, the implementation cannot branch on `T`, inspect `T`, or call type-specific methods on `T`. It is forced to operate only on container structure.

Add trait bounds only for capabilities the function actually requires.

```rust
fn render_blind<T>(_value: &T) {}
fn render<T: Display>(value: &T) -> String { value.to_string() }
```

Trait bounds are call-site rejection. The function does not exist for types that do not satisfy the bound.

## VII. Ownership Is Coordination

One component MUST own each mutable resource. If two components must "agree" on the state of a resource, the architecture MUST be rewritten.

Ownership hierarchy is the coordination mechanism. Other components borrow or communicate with the owner; they do not co-own domain truth.

`Arc<Mutex<T>>`, `Arc<RwLock<T>>`, `Rc<RefCell<T>>`, `Mutex<T>`, and `RwLock<T>` are strong signals that ownership was not drawn correctly. Core domain state MUST be rewritten into one owner plus borrowed views or typed handles.

Retry loops inside core logic indicate a consensus failure: the system is trying to synchronize with itself. Boundary retries adapt to external unreliability. Internal retries indicate broken ownership.

### A. Async Spawn Boundaries

A spawned task is an ownership boundary. Cross-task mutation MUST be modeled as transferred ownership or typed coordination handles, not `&mut` borrows.

Core domain state, policy state, authority state, typestate carriers, proof-bearing values, and business invariants MUST NOT be shared across spawned tasks through `Arc<Mutex<_>>`, `Arc<RwLock<_>>`, `Mutex<_>`, `RwLock<_>`, or equivalent wrappers.

When multiple async tasks need to affect the same mutable value and any participating path may `await`, exactly one task MUST own that value. Other tasks MUST communicate with that owner through typed handles.

Typed coordination handles MUST use:

- `mpsc::Sender<Command>` for commands,
- `oneshot::Sender<Reply>` for per-call replies,
- `watch::Receiver<Snapshot>` for read-mostly latest state,
- `broadcast::Receiver<Event>` for event fanout,
- `Semaphore` permits for capacity or leases,
- `JoinSet` for task supervision and shutdown.

Strict-surface APIs MUST NOT expose shared mutable ownership. They MUST expose handles, commands, snapshots, permits, and join rights.

Locks are not state machines. A lock-protected value MUST NOT be the canonical owner of domain truth when an owner task can exist.

### B. Narrow Exception: Runtime Coordination Cells

`Arc<Mutex<_>>` and `Arc<RwLock<_>>` are permitted only in explicitly named runtime-shell or compatibility-quarantine modules, and only when all of the following hold:

1. The guarded value is coordination metadata, not domain truth.
2. The guarded value does not encode policy, permission, typestate, phase, or business invariants.
3. No lock is held across `.await`.
4. The guard never escapes the module.
5. The `Arc` never appears in a strict-surface API.
6. No nested locking or lock-order graph exists.
7. The cell is replaceable in principle by an owner task and is retained only as a localized runtime convenience.

Permitted examples: pending-response maps, cancellation registries, hot cache shards, metrics accumulators, lazily initialized runtime slots.

Forbidden examples: workflow state, approval policy, capability possession, typestate progression, session authority, or any value whose meaning defines application behavior.

## VIII. Providers Expose Mechanism; Callers Decide Policy

Providers MUST expose mechanisms. Callers MUST enforce policies.

A provider MUST NOT silently return fallback data when the caller asked for a specific resource. Hidden defaults are policy decisions.

If a function can return fallback data or real data, the type system MUST distinguish those outcomes. Otherwise the function has embedded policy inside data access.

Loading and reading MUST be separated. Separate loading from proof of residency.

```rust
pub struct ResidentTexture<'a>(&'a Texture);

impl TextureManager {
    pub fn load(&mut self, id: AssetId) -> Result<(), LoadError>;
    pub fn get(&self, id: AssetId) -> Result<ResidentTexture<'_>, LookupError>;
}
```

`load` mutates. `get` proves residency through `&self`. The provider reports reality; the caller decides what to do with failure.

## IX. Capability Tokens

Operations valid only within a phase or under a specific authority MUST require a phase-bound or authority-bound token. Use a zero-sized witness type with a private constructor.

```rust
pub struct RenderPassToken(());

pub fn submit_draw_call(_permit: &RenderPassToken, mesh: Mesh) {
    // ...
}
```

Calls MUST require possession of the token. Scope MUST determine which operations are legal.

### A. Token Naming

Capability tokens MUST be named for the exact authority they grant, not for the subsystem they happen to touch.

Use `NetworkEgressGrant` or `SystemExecReadPermit` rather than generic nouns such as `NetworkClient`.

When storing token availability, wrap the token in a two-variant enum. The denial variant MUST describe the exact restricted perimeter, not merely a generic absence.

```rust
pub struct NetworkEgressGrant { _private: () }

pub enum NetworkEgressPolicy {
    EgressDenied,
    EgressPermitted(NetworkEgressGrant),
}
```

The caller MUST pattern-match on the consequence before obtaining the token.

### B. Token Recovery on Retryable Failure

Capability tokens are affine. They are not duplicable.

If an operation consumes a capability token and fails in a retryable way, the failure path MUST return recoverable authority only when absolutely zero side effects occurred.

If any side effect occurred, even partial, the token MUST NOT be returned.

Recovery outcome types MUST be marked `#[must_use]`.

Do not return a raw token by default. Returning raw authority decouples authority from intent and enables retargeting. Return a suspended continuation that fuses the token with the original operation.

```rust
pub struct EgressGrant(());
pub struct Payload(String);

pub struct SuspendedTransmit {
    payload: Payload,
    grant: EgressGrant,
}

#[must_use]
pub enum TransmitOutcome {
    Transmitted,
    Retryable(SuspendedTransmit),
    PermanentFailure,
}

impl SuspendedTransmit {
    pub fn retry(self) -> TransmitOutcome {
        transmit_internal(self.payload, self.grant)
    }

    pub fn abort(self) -> EgressGrant {
        self.grant
    }
}
```

On success, permanent failure, or any partial side effect, the token is consumed. `SuspendedTransmit` MUST NOT derive `Clone`.

## X. Boundary Discipline

Boundaries are the immediate deserialization layer, FFI adapter, and external API handler. Boundaries translate foreign ambiguity into strict internal types.

`Option`, `bool`, parse failures, and foreign schemas MUST be collapsed in the exact boundary function that first receives them. Parsing failures MUST be translated into strict, domain-specific `Result` values immediately at ingestion.

Boundary and core types MUST NOT contain `Option<T>`, pseudo-optionals, or bare `bool`. Constructors MUST yield fully valid values.

Passing raw optionality or booleans through multiple boundary functions spreads ambiguity. They MUST be eliminated in the first boundary function that touches them.

For configuration, explicit operational modes such as `OfflineOnly | ProviderBacked(T)` MUST be used instead of `Option<T>` fields or `enabled: bool`.

For policy and authority, finite ordered variants plus total merge rules MUST be used. Wildcard bypass variants and `allow_all: bool` are forbidden.

### A. Deserialization Mandate

External formats such as JSON and TOML are inherently loose. That looseness MUST NOT survive parsing.

`#[serde(default)]`, `#[serde(deserialize_with)]`, custom deserializers, or explicit `TryFrom` conversions MUST be used as needed so that parsing yields fully inhabited, `Option`-free internal values immediately.

If a missing field is invalid and no default is semantically correct, deserialization MUST fail.

The domain type is the schema.

## XI. Module Privacy and Construction Control

If an invariant cannot be expressed in the public type signature alone, construction MUST be restricted by module privacy.

Private fields, sealed traits, and smart constructors such as `fn new() -> Result<Self, Error>` MUST guarantee that downstream code cannot construct invalid values.

The `_proof: ()` pattern is one example. Use any equivalent privacy technique that prevents external construction of invalid states.

## XII. Assertions and Panic-Based Design

`assert!`, `debug_assert!`, `unwrap()`, `expect()`, and `unreachable!()` do not prevent invalid states. They detect invalid states that the type design already permitted.

In core logic, these are signs that the representation is too weak. Strengthen the representation instead.

## XIII. Rust Approximations

Rust is not dependently typed and provides escape hatches such as `unsafe` and interior mutability. That does not weaken the contract.

Use these approximations:

| Ideal | Rust approximation |
|---|---|
| Typestate | Newtypes plus consuming transitions (`fn next(self) -> Next`) |
| Linear types | Affine ownership, move semantics, `#[must_use]` |
| Parametricity | Generics with minimal trait bounds |
| Bounded polymorphism | Trait bounds and `where` clauses |
| Capability tokens | Zero-sized witness types with private constructors |
| State as location | Separate containers per state, or enums when co-located |

If Rust cannot forbid an invalid state completely, the interface MUST still make that state difficult to construct and impossible to use accidentally. Imperfect enforcement requires a harder interface, not a weaker one.

## XIV. Quick Review Test

During review, do not ask, "What happens if someone passes the wrong data?"

Ask, "How do I make passing the wrong data a compile error?"

Compile-time friction is cheaper than runtime desynchronization. If the borrow checker resists the design, redesign the architecture.

## XV. Disallowed Patterns

| Pattern | Why it is forbidden |
|---|---|
| `Option<T>` fields or pseudo-optional variants | Encodes absence instead of a domain fact |
| Bare `bool` fields or parameters | Compresses meaning into unlabeled bits |
| Wildcard policy variants (`Any`, `All`, `Unrestricted`, `AllowAll`) | Creates a universal escape hatch |
| False optionality (`Option<T>` accepted when `T` is required) | Lies about the contract |
| Sentinel values | Uses in-band signaling instead of types |
| Two-phase initialization | Exposes partially initialized state |
| `.unwrap()` / `expect()` / `unreachable!()` in core logic | Replaces type obligations with runtime panic |
| `RefCell`-driven design | Defers a compile-time ownership problem to runtime |
| `Arc<Mutex<...>>` as domain architecture | Fragments ownership and creates consensus problems |
| `.clone()`-driven design | Hides ownership failures instead of fixing them |

## XVI. Final Standard

The standard is a codebase where `cargo check` is evidence that invalid states, illegal transitions, and unauthorized operations have been designed out of the reachable program.
