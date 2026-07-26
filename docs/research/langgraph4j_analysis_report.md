# langgraph4j/langgraph4j — 

> 2026-07-07  
> v1.8.20 ()  
>  Mora 

---

## 1. 

|  |  |
|------|------|
| **** | [langgraph4j/langgraph4j](https://github.com/langgraph4j/langgraph4j) |
| **** | Java 17+ |
| **** | Java ** Agent LLM ** Python LangGraph  Java  |
| **** | v1.8.20 (2026-06-27) |
| **** | LangChain4jSpring AIOpenTelemetry |
| **** | Memory / MySQL / PostgreSQL / Redis / DynamoDB / Hazelcast / CockroachDB / Oracle |
| **License** | MIT |

****LangGraph4j  Agent **StateGraph** Agent 

---

## 2. 

### 2.1 

```
langgraph4j/
 langgraph4j-core/           # StateGraphCompiledGraphAgentStateChannelCheckpoint
 langgraph4j-{mysql|postgres|redis|...}-saver  # 
 langchain4j/                # LangChain4j  + ReACT AgentExecutor
 spring-ai/                    # Spring AI  + AgentExecutor
 studio/                       #  Web UISpring Boot/Jetty/Quarkus
 how-tos/                      # Jupyter persistencetime-travelsubgraph
 javelit/                      # SpinnerMultiSelectPlantUML
```

### 2.2 

LangGraph4j  **** `(State, Schema, Node, Edge)`

|  |  |  |
|------|------|------|
| **AgentState** | `Map<String, Object>` |  |
| **Schema (Channels)** | `Map<String, Channel<?>>` | ****// |
| **Node** | `NodeAction<S>` / `AsyncNodeAction<S>` |  `Map<String, Object>`  |
| **Edge** | `EdgeAction<S>` / `AsyncEdgeAction<S>` |  |

### 2.3 →→

```
StateGraph ()
     addNode / addEdge / addConditionalEdges
    
compile( CompileConfig )  →  CompiledGraph ()
     checkpointSaver?  recursionLimit?  interruptsBefore/After?
    
stream( inputs, RunnableConfig )  →  AsyncGenerator<NodeOutput>
     NodeOutput(nodeId, state)
    
CheckpointSaver.put()  ←  ← 
```

### 2.4 Channel + Reducer

 LangGraph4j  `Map`  **Schema** 

```java
//  Schemamessages 
Map<String, Channel<?>> SCHEMA = Map.of(
    "messages", Channels.appender(ArrayList::new),  // 
    "counter",  Channels.reducer( (old, val) -> val )  // 
);
```

`Channel` 
- **Reducer**: `(_old, _new) -> merged` — 
- **Default**: `Supplier<T>` — 
- ****: `MARK_FOR_RESET``MARK_FOR_REMOVAL`

> **** Actor Model ******Event Sourcing**“”Schema fold

---

## 3. 

### 1Checkpoint /  + 

****: `CompiledGraph.getStateHistory()`, `updateState()`, `CheckpointSaver` 

```java
// 
Collection<StateSnapshot> history = compiledGraph.getStateHistory(config);

// 
RunnableConfig newConfig = compiledGraph.updateState(config, Map.of("messages", newMessages), "asNode");

// 
compiledGraph.stream(GraphInput.resume(), newConfig);
```

****
-  `RunnableConfig`  `threadId` + `checkpointId` + `nextNode`****
- `CheckpointSaver`  7+ 
- `StateSnapshot` config
-  ISSUE  `parentConfig` Python/JS 

### 2Interrupt / Human-in-the-Loop

****: `CompileConfig.interruptsBefore()`, `CompiledGraph.shouldInterruptBefore()`

```java
var compiledGraph = stateGraph.compile(
    CompileConfig.builder()
        .interruptsBefore("execute_tool")   // 
        .interruptsAfter("call_llm")         // LLM
        .build()
);

//  InterruptionMetadata
for (var item : compiledGraph.stream(inputs)) {
    if (item instanceof InterruptionMetadata im) {
        //  resume
        var newConfig = compiledGraph.updateState(im.config(), approvedState);
        compiledGraph.stream(GraphInput.resume(), newConfig);
    }
}
```

****
- ****
-  `updateState()`  `GraphInput.resume()` 
-  `CheckpointSaver` ****OpenHuskyAgent 

### 3Command  —  + 

****: `Command.java`, `StateGraph.addNode(id, AsyncCommandAction, mappings)`

```java
// 1)   2) 
public interface AsyncCommandAction<S extends AgentState> {
    CompletableFuture<Command> apply(S state, RunnableConfig config);
}

// Command 
record Command(String gotoNode, Map<String, Object> update) {}
```

****
- Command ****
-  LangGraph  `Command` LangGraph 1.0 ********
- Mora  `orchestrate` ****

### 4Subgraph / 

****: `SubStateGraphNode`, `SubCompiledGraphNode`, `ProcessedNodesEdgesAndConfig.process()`

```java
// 
stateGraph.addNode("sub", subGraph);              // 1.  StateGraph 
stateGraph.addNode("sub", compiledSubGraph);       // 2.  CompiledGraph 
// 3.  NodeAction 
```

****
- ** AgentState** ID `subgraph::nodeId`
- `process()` 
-  resume`SUBGRAPH_RESUME_UPDATE_DATA` 

### 5ParallelNode / 

****: `ParallelNode`, `CompiledGraph`  target 

```java
//  ParallelNode
var parallelNode = new ParallelNode<>(sourceId, actions, channels);
//  Channel.Reducer 
```

****
- `unsupportedConditionalEdgeOnParallelNode`
- `illegalMultipleTargetsOnParallelNode`
-  Schema  Reducer `Channels.appender()` 

### 6Hooks / 

****: `NodeHooks`, `EdgeHooks`, `LG4JLoggable`

```java
stateGraph
    .addBeforeCallNodeHook("call_llm", (nodeId, state, config) -> { log(); return state; })
    .addAfterCallNodeHook((nodeId, state, config, result) -> { metrics(); return result; })
    .addWrapCallNodeHook((nodeId, state, config, action) -> {
        // OpenTelemetry span 
        return action.apply(state, config);
    });
```

****
-  Hook
-  Before/After/WrapCall 
-  OpenTelemetry 

---

## 4.  Mora 

### 1 Schema/Channel 

****Mora  `with`  `orchestrate` 

**LangGraph4j **
```java
Map<String, Channel<?>> SCHEMA = Map.of(
    "messages", Channels.appender(ArrayList::new),
    "context",  Channels.reducer( (old, val) -> merge(old, val) )
);
```

**Mora **
```mora
// 
state_schema MyState {
    messages: [Message] @append,      // 
    context: Context @merge,          // 
    counter: int @replace,             // 
    flags: Set<String> @union,        // 
}

//  orchestrate 
orchestrate MyFlow(state: MyState) {
    node fetcher -> state {          //  Map  Schema 
        return { messages: [newMsg] } //  messages
    }
}
```

### 2 Checkpoint / 

****Mora  `record`/`replay`********

**Mora **
```mora
// 1. 
orchestrate MyFlow(state: MyState) @checkpoint(saver: "postgres", thread: "session_id") {
    ...
}

// 2.  API
val history = flow.state_history(thread_id);  // 
flow.update_state(thread_id, checkpoint_id, { messages: [user_edit] });
flow.resume(thread_id, checkpoint_id);

// 3. 
node human_approval @interrupt_after {
    //  update_state
}
```

### 3 Command 

****Mora  `orchestrate` 

**Mora **
```mora
node router -> state, next: string {
    let result = ai.chat("", state);
    return state, next: result.choice;  // 
}

// 
node router -> Command {
    return Command {
        update: { messages: [...] },
        goto: result.choice
    };
}
```

### 4Thread

****Mora  `orchestrate` /

**LangGraph4j **`RunnableConfig.threadId` 

**Mora **
```mora
//  thread_id 
val session = flow.spawn_thread("user_123");
val result = session.run({ query: "hello" });
val history = session.checkpoints();  // 
```

### 5

**LangGraph4j **`compile()`  orphaned nodesmissing entry pointsduplicate edges 

**Mora ** `orchestrate`  Rust 
- 
- 
- `recursionLimit`

### /

|  |  |
|------|------|
| **Java CompletableFuture ** | Mora  Rust `async/await` + `stream` |
| **PlantUML/Mermaid ** | Mora  |
| **Spring Boot/Jetty ** |  Java Mora  Web  TUI  WebAssembly |
| ** ReACT AgentExecutor** | LangGraph4j  ReACT Mora  `ai.chat` + tool  |

---

## 5. 17

### 5.1  LangGraph (Python ) 

|  | LangGraph (Python) | LangGraph4j (Java) |
|------|---------------------|----------------------|
|  |  Python dict + TypedDict |  `AgentState` + `Channel` Schema + Reducer |
|  | `asyncio` / `async` | `CompletableFuture` + `AsyncGenerator`java-async-generator  |
|  |  `MemorySaver` / `PostgresSaver` | 8  |
|  |  |  |
|  |  LangChain |  LangChain4j **** Spring AI |
|  |  Mermaid | PlantUML + Mermaid +  Studio |

### 5.2  AIOS / mini-swe-agent / loongclaw 

|  |  |  LangGraph4j  |
|------|----------|--------------------------|
| **AIOS** | LLM  | AIOS  Agent ****CPU time sharingLangGraph4j  Agent ****control flow graphMora  AIOS  + LangGraph4j  |
| **mini-swe-agent** |  Agent | edit/search/shellLangGraph4j mini-swe-agent **** LangGraph4j  |
| **loongclaw** |  Agent  | /LangGraph4j ****LangGraph4j  |
| **ChatDev** |  | CEO/CTO/ProgrammerLangGraph4j ******** Mora  |
| **agents-cli** |  |  LLM  CLI LangGraph4j  CLIMora  `exec.bash`  agents-cli LangGraph4j  |

### 5.3 LangGraph4j 

1. **** `compile()` 
2. **Channel ** `Reducer`/`Default`/`Appender` 
3. ****`interruptsBefore`/`interruptsAfter`  Checkpoint 
4. ** Studio** Spring Boot/Jetty/Quarkus 
5. **Java ** `java-async-generator`  Python `yield`  JVM 

---

## 6. Mora 

|  |  | Mora  |  |
|--------|------|---------------|------|
|  P0 | Channel/Schema + Reducer |  `@append`, `@merge`, `@replace` |  |
|  P0 | Checkpoint +  | `orchestrate @checkpoint(saver: ...)` + `thread`  |  Agent  |
| 🟡 P1 | Command  |  `Command { update, goto }` |  |
| 🟡 P1 | Thread  | `flow.spawn_thread()` / `session.checkpoints()` | / |
| 🟢 P2 |  | `orchestrate`  |  |
| 🟢 P2 | Hooks  | `@before_node`, `@after_node`, `@wrap`  | AOP  |

---

> ****Orchestrator Agent  
> ****GitHub langgraph4j/langgraph4j main READMECHANGELOGhow-tos notebooks  
> ****StateGraph.javaCompiledGraph.javaAgentState.javaChannel.javaAgentExecutor.java + 
