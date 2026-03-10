# Sprint 015: CALM-Inspired SDR Memory with Zero Prompt Injection

## Objective
Replace Mimir's dense float embedding RAG pipeline with a dual-system SDR (Sparse Distributed Representation) architecture inspired by the CALM framework (Dec 2025). System 1 provides sub-millisecond in-memory recall via binary SDRs. System 2 consolidates via Qdrant BQ. Memory influences Odin's behavior structurally (routing, model selection) with zero text injected into the prompt.

## Scope
- **Mimir**: Replace Ollama embedding + Qdrant float search with ONNX in-process embedding (all-MiniLM-L6-v2) + binarization + dual-system SDR retrieval
- **Odin**: Replace text-stuffing RAG with event-based memory consumption (zero prompt injection)
- **ygg-domain**: New EngramEvent/EngramTrigger types, SdrConfig
- **ygg-store**: Updated PG schema for sdr_bits + trigger columns
- **Not in scope**: Muninn (code search unchanged), Huginn (indexer unchanged)

## Hardware Targets
- Munin (Intel Core Ultra 185H, 48GB DDR5): Odin + Mimir + ONNX Runtime
- Hugin (AMD Ryzen 7 255, 46GB RAM): Huginn + Muninn (unchanged)
- Hades (Intel N150, 32GB): Qdrant (engrams_sdr collection + existing code_chunks)

## Performance Targets
- ONNX embedding: < 30ms on Munin CPU (all-MiniLM-L6-v2, 384-dim)
- SDR binarization: < 1ms
- System 1 recall (in-memory): < 1ms for 1000 engrams
- System 2 recall (Qdrant BQ): < 15ms
- Total store latency: < 50ms (excl. PG insert)
- Total recall latency: < 50ms
- Memory: < 10MB for SDR index (10K engrams × 32 bytes)

## Acceptance Criteria
- [ ] Mimir no longer depends on ygg-embed or Ollama for embedding
- [ ] ONNX model loads and produces 384-dim embeddings in-process
- [ ] SDR binarization produces deterministic 256-bit representations
- [ ] In-memory SdrIndex returns correct top-K by Hamming distance
- [ ] POST /api/v1/recall returns Vec<EngramEvent> with NO cause/effect text
- [ ] POST /api/v1/store uses ONNX + SDR pipeline
- [ ] Qdrant engrams_sdr collection uses 256-dim dot product with BQ
- [ ] Odin's system prompt contains ZERO memory text
- [ ] Odin's routing is influenced by memory events
- [ ] Old POST /api/v1/query remains as deprecated backward-compat endpoint
- [ ] LSH module and lsh_buckets table deleted
- [ ] cause_embedding column dropped from engrams table
