# NgenOrca Enterprise Architecture Diagrams

This document provides a complete enterprise architecture view of NgenOrca using:

- **TOGAF-style architecture domains** (Business, Application, Data, Technology)
- **Cross-cutting Security and Governance views**
- **Alternative viewpoints** (C4-style and ArchiMate-style layering)

Use this as a canonical architecture reference for security review, platform planning, and audit readiness.

---

## 1) TOGAF Functional View (Business + Capability)

### 1.1 Business Capability Map

```mermaid
flowchart TB
  subgraph Experience[User Experience Capabilities]
    C1[Omnichannel Conversational Access]
    C2[Identity-Aware Personalization]
    C3[Operational Self-Service API]
  end

  subgraph Intelligence[AI Intelligence Capabilities]
    C4[Intent Classification & Routing]
    C5[Provider Abstraction & Model Selection]
    C6[Quality Gate & Escalation]
  end

  subgraph Memory[Memory Capabilities]
    C7[Working Memory]
    C8[Episodic Memory]
    C9[Semantic Memory]
  end

  subgraph Trust[Trust & Security Capabilities]
    C10[Delegated Authentication via Trusted Proxy]
    C11[Session & Rate-Limit Protection]
    C12[Sandboxed Tool/Plugin Execution]
  end

  subgraph Operability[Run & Operate Capabilities]
    C13[Metrics & Tracing]
    C14[Durable Event Logging]
    C15[CI/CD & Release Automation]
  end

  C1 --> C4
  C2 --> C7
  C4 --> C5
  C5 --> C6
  C6 --> C7
  C7 --> C8
  C8 --> C9
  C10 --> C11
  C11 --> C1
  C12 --> C5
  C13 --> C15
  C14 --> C13
```

### 1.2 Business Interaction / Value Stream (Request to Response)

```mermaid
sequenceDiagram
  autonumber
  participant U as End User
  participant IdP as Authelia/EntraID
  participant RP as Reverse Proxy
  participant GW as NgenOrca Gateway
  participant OR as Orchestrator
  participant LLM as Provider/Model
  participant MEM as Memory System

  U->>RP: Request/Message
  RP->>IdP: Authenticate user
  IdP-->>RP: Identity + claims
  RP->>GW: Forward request + trusted headers
  GW->>GW: Auth middleware (TrustedProxy)
  GW->>OR: Classify and route
  OR->>MEM: Build context
  OR->>LLM: Inference request
  LLM-->>OR: Response
  OR->>MEM: Update working/episodic/semantic memory
  OR-->>GW: Final response
  GW-->>RP: Response + request-id
  RP-->>U: Final response
```

---

## 2) TOGAF Application Architecture View

### 2.1 Application Component Diagram

```mermaid
flowchart LR
  subgraph Edge[Identity + Edge Layer]
    PRX[Reverse Proxy / WAF]
    IDP[Authelia or EntraID]
  end

  subgraph Core[NgenOrca Core Runtime]
    GWT[Gateway API + WS]
    AUTH[Auth Middleware]
    RL[Rate Limiter]
    RID[Request-ID Middleware]
    ORCH[Hybrid Orchestrator]
    PROV[Provider Registry]
    PLUG[Plugin Registry]
    SES[Session Manager]
    MET[Metrics]
  end

  subgraph DataServices[Stateful Services]
    BUS[Durable Event Bus]
    MEMW[Working Memory]
    MEME[Episodic Memory]
    MEMS[Semantic Memory]
    IDM[Identity Manager]
  end

  subgraph External[External AI + Channels]
    L1[Anthropic/OpenAI/Azure/Google]
    L2[Ollama/Custom Providers]
    CH[Telegram/Slack/Discord/WhatsApp/Teams/etc]
  end

  U1((Clients)) --> PRX
  PRX --> IDP
  IDP --> PRX
  PRX --> GWT

  GWT --> AUTH
  AUTH --> RL
  RL --> RID
  RID --> ORCH
  ORCH --> PROV
  ORCH --> PLUG
  ORCH --> SES
  ORCH --> BUS
  ORCH --> MEMW
  ORCH --> MEME
  ORCH --> MEMS
  ORCH --> IDM
  ORCH --> MET

  PROV --> L1
  PROV --> L2
  PLUG --> CH
```

### 2.2 Functional Service Boundaries

```mermaid
flowchart TB
  subgraph APIGateway[Gateway Service Boundary]
    A1[HTTP API]
    A2[WebSocket API]
    A3[Health + Metrics]
  end

  subgraph ControlPlane[AI Control Plane Boundary]
    B1[Intent Classification]
    B2[Routing Strategy]
    B3[Quality Evaluation]
    B4[Escalation Logic]
  end

  subgraph MemoryPlane[Memory Plane Boundary]
    C1[Context Assembly]
    C2[Conversation Persistence]
    C3[Knowledge Consolidation]
  end

  subgraph IntegrationPlane[Integration Plane Boundary]
    D1[LLM Provider Abstraction]
    D2[Plugin SDK + Tooling]
    D3[Channel Adapters]
  end

  A1 --> B1
  A2 --> B1
  B4 --> C1
  C3 --> B2
  B2 --> D1
  B2 --> D2
  D2 --> D3
```

---

## 3) TOGAF Data Architecture View

### 3.1 Conceptual Data Model

```mermaid
erDiagram
  USER ||--o{ DEVICE : owns
  USER ||--o{ SESSION : starts
  USER ||--o{ CHANNEL_IDENTITY : maps
  SESSION ||--o{ MESSAGE : contains
  SESSION ||--o{ EVENT : emits
  USER ||--o{ EPISODIC_ENTRY : has
  USER ||--o{ SEMANTIC_FACT : has
  SESSION ||--o{ WORKING_MEMORY_ENTRY : caches

  USER {
    string user_id
    string display_name
    string role
    datetime created_at
    datetime last_seen
  }

  DEVICE {
    string device_id
    string attestation_type
    string trust_level
    datetime paired_at
  }

  CHANNEL_IDENTITY {
    string channel_kind
    string handle
    string trust_level
  }

  SESSION {
    string session_id
    string channel
    string state
    string model
    int message_count
    int tokens_used
    datetime created_at
    datetime last_active
  }

  MESSAGE {
    string message_id
    string direction
    string content_type
    datetime timestamp
  }

  EVENT {
    string event_id
    string payload_type
    datetime timestamp
  }

  WORKING_MEMORY_ENTRY {
    string role
    string content
    int estimated_tokens
    datetime timestamp
  }

  EPISODIC_ENTRY {
    int id
    string content
    string channel
    datetime timestamp
  }

  SEMANTIC_FACT {
    int id
    string fact
    float confidence
    datetime updated_at
  }
```

### 3.2 Data Lifecycle and Retention

```mermaid
flowchart LR
  IN[Inbound Message] --> WM[Working Memory]
  WM -->|Session progress| EP[Episodic Memory]
  EP -->|Consolidation jobs| SM[Semantic Memory]
  WM -->|Immediate event| EV[Event Log]
  EP -->|Retention policy| PR1[Prune]
  SM -->|Conflict/decay policy| PR2[Refine]
  EV -->|Retention policy| PR3[Prune]
```

---

## 4) TOGAF Technology Architecture View

### 4.1 Runtime and Infrastructure Topology

```mermaid
flowchart TB
  subgraph ClientZone[Client Zone]
    C[Browser / API Client / Chat Channel]
  end

  subgraph EdgeZone[Edge Zone]
    WAF[WAF / Firewall]
    RP[Reverse Proxy]
    IDP[Authelia or EntraID]
  end

  subgraph AppZone[Application Zone]
    NG[NgenOrca Gateway Container or Binary]
    PL[Plugin Processes/Adapters]
  end

  subgraph DataZone[Data Zone]
    DB1[(SQLite: Event Log)]
    DB2[(SQLite: Episodic/Semantic)]
    VOL[(Encrypted Volume / Backup)]
  end

  subgraph ExternalZone[External Services]
    LLM[(Cloud or Local LLM)]
    OBS[(OTLP / Metrics Backend)]
  end

  C --> WAF --> RP
  RP <--> IDP
  RP --> NG
  NG <--> PL
  NG --> DB1
  NG --> DB2
  DB1 --> VOL
  DB2 --> VOL
  NG --> LLM
  NG --> OBS
```

### 4.2 Network Trust Boundaries (Direct Access Rejection)

```mermaid
flowchart LR
  Internet[Client Networks] -->|443 only| Proxy[Identity-aware Reverse Proxy]
  Proxy -->|Internal network only| Backend[NgenOrca Backend]
  Internet -. blocked .-> Backend
  Monitor[Monitoring Network] -->|Restricted health/metrics path| Backend
```

---

## 5) Security Architecture View (Cross-cutting)

### 5.1 Control Layering

```mermaid
flowchart TB
  S1[Identity Provider Controls\nSSO, MFA, Conditional Access]
  S2[Edge Controls\nWAF, TLS termination, header normalization]
  S3[Gateway Controls\nTrustedProxy auth, rate limits, request-id, session controls]
  S4[Execution Controls\nSandbox policies, plugin permissions]
  S5[Data Controls\nRetention, encryption, backup integrity]
  S6[Detection Controls\nMetrics, logs, traces, alerts]

  S1 --> S2 --> S3 --> S4 --> S5 --> S6
```

### 5.2 Threat-to-Control Mapping (Simplified)

```mermaid
flowchart LR
  T1[Header Spoofing] --> C1[Proxy strip/re-set headers + trusted source allowlist]
  T2[Direct Backend Reachability] --> C2[No public backend exposure + firewall deny]
  T3[Abuse/DoS] --> C3[Rate limits + WS caps + quotas]
  T4[Prompt/Tool Abuse] --> C4[Sandbox + plugin permission boundaries]
  T5[Data Over-retention] --> C5[Retention jobs + documented policy]
  T6[Detection Blindspots] --> C6[Request IDs + metrics + tracing + alerting]
```

---

## 6) Alternative Enterprise Architecture Views

## 6.1 C4-Style Context and Container Views

### C4 Level 1 (System Context)

```mermaid
flowchart LR
  User[End User]
  Admin[Ops/Admin]
  NgenOrca[NgenOrca System]
  IdP[Authelia or EntraID]
  LLM[LLM Providers]
  Channels[Messaging Channels]
  Obs[Observability Stack]

  User --> NgenOrca
  Admin --> NgenOrca
  NgenOrca --> IdP
  NgenOrca --> LLM
  NgenOrca --> Channels
  NgenOrca --> Obs
```

### C4 Level 2 (Container)

```mermaid
flowchart TB
  subgraph NgenOrcaSystem[NgenOrca]
    C1[Gateway Container]
    C2[Memory/Identity Stores (SQLite)]
    C3[Plugin Runtime]
  end
  IdP[Authelia or EntraID]
  Proxy[Reverse Proxy]
  LLM[LLM Provider(s)]
  User[Client]

  User --> Proxy
  Proxy <--> IdP
  Proxy --> C1
  C1 --> C2
  C1 --> C3
  C1 --> LLM
```

## 6.2 ArchiMate-Style Layered View (Conceptual)

```mermaid
flowchart TB
  subgraph BusinessLayer[Business Layer]
    B1[Conversational Assistance Service]
    B2[Identity-Assured Access]
    B3[Operational Monitoring]
  end

  subgraph ApplicationLayer[Application Layer]
    A1[Gateway Application Service]
    A2[Orchestration Application Service]
    A3[Memory Application Service]
    A4[Identity Application Service]
  end

  subgraph TechnologyLayer[Technology Layer]
    T1[Proxy/IdP Technology Service]
    T2[Container Runtime]
    T3[SQLite Data Technology Service]
    T4[Telemetry Stack]
  end

  B1 --> A1
  B2 --> A4
  B3 --> A1
  A1 --> T1
  A2 --> T2
  A3 --> T3
  A1 --> T4
```

---

## 7) TOGAF to Alternative Mapping

| TOGAF Domain | Primary Diagram(s) in this doc | C4 Equivalent | ArchiMate Equivalent |
|---|---|---|---|
| Business Architecture | 1.1, 1.2 | Context + user goals | Business Services/Processes |
| Application Architecture | 2.1, 2.2 | Container/Component | Application Services/Functions |
| Data Architecture | 3.1, 3.2 | Container data relationships | Data Objects and Flows |
| Technology Architecture | 4.1, 4.2 | Deployment and infra context | Technology Services/Nodes |
| Security (cross-cutting) | 5.1, 5.2 | Threat overlays | Motivation/Constraint overlays |

---

## 8) Implementation and Migration View (Roadmap)

```mermaid
gantt
  title Enterprise Architecture Implementation Roadmap
  dateFormat  YYYY-MM-DD
  section Immediate Controls
  Direct-access rejection guardrails      :a1, 2026-03-10, 10d
  WS authorization boundaries             :a2, 2026-03-12, 14d
  Dependency + vulnerability automation   :a3, 2026-03-10, 7d

  section Hardening
  Trusted proxy defense-in-depth          :b1, 2026-03-24, 14d
  Webhook signature verification          :b2, 2026-03-26, 21d
  Audit logging baseline                  :b3, 2026-03-24, 14d

  section Audit Readiness
  SBOM + provenance + signing             :c1, 2026-04-12, 14d
  Control matrix + policies               :c2, 2026-04-12, 21d
  DR/tabletop validation                  :c3, 2026-04-18, 14d
```

---

## 9) Notes for Architecture Review Board

- **Enterprise default posture:** `TrustedProxy` with no direct backend exposure.
- **Functional emphasis:** Orchestration quality + memory continuity across channels.
- **Technical emphasis:** strict trust boundaries, observability, and release integrity.
- **Decision checkpoints:** identity boundary enforcement, event visibility boundaries, and supply-chain attestation.
