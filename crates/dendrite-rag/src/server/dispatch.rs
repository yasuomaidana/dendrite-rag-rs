//! Method routing of `dendrite-rag serve`: `dispatch` picks a method family and every method has a
//! small handler of its own.

use super::*;

type Reply = Result<Value, RpcError>;

fn unknown(method: &str) -> RpcError {
    RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {method}"))
}

impl Server {
    pub(super) fn dispatch(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "health" | "embedders" | "settings" => self.meta_method(method, params),
            "index" | "sync" | "pull" | "push" | "status" | "purge" => self.index_method(method, params),
            "search" | "context" | "chunks.get" | "rank" | "grep" | "ask" => {
                self.retrieval_method(method, params)
            }
            "symbols.definitions" | "symbols.references" | "history.index" | "history.search" => {
                self.symbol_method(method, params)
            }
            m if m.starts_with("graph.") => self.graph_method(m, params),
            m if m.starts_with("quality.") => quality_method(m, params),
            _ => Err(unknown(method)),
        }
    }

    fn meta_method(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "health" => Ok(rpc::health()),
            "embedders" => Ok(json!({ "backends": loom_rag::embed::backends() })),
            "settings" => self.rpc_settings(params),
            _ => Err(unknown(method)),
        }
    }

    fn index_method(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "index" => self.rpc_index(params),
            "sync" => self.rpc_sync(params),
            #[cfg(feature = "postgres")]
            "push" => self.rpc_push(params),
            "pull" => self.rpc_pull(params),
            "status" => self.rpc_status(params),
            "purge" => self.rpc_purge(params),
            _ => Err(unknown(method)),
        }
    }

    fn retrieval_method(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "search" => self.rpc_search(params),
            "context" => self.rpc_context(params),
            "chunks.get" => self.rpc_chunks(params),
            "rank" => self.rpc_rank(params),
            "grep" => self.rpc_grep(params),
            "ask" => self.rpc_ask(params),
            _ => Err(unknown(method)),
        }
    }

    fn symbol_method(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "symbols.definitions" | "symbols.references" => self.rpc_symbols(method, params),
            "history.index" => self.rpc_history_index(params),
            "history.search" => self.rpc_history_search(params),
            _ => Err(unknown(method)),
        }
    }

    fn graph_method(&mut self, method: &str, params: Value) -> Reply {
        match method {
            "graph.communities" => self.rpc_communities(params),
            "graph.god_nodes" => self.rpc_god_nodes(params),
            "graph.path" => self.rpc_graph_path(params),
            "graph.explain" => self.rpc_explain(params),
            _ => Err(unknown(method)),
        }
    }

    fn rpc_settings(&mut self, params: Value) -> Reply {
        let p: RootParams = rpc::params(params)?;
        let root = Path::new(&p.root);
        let file = crate::settings::Settings::path(root);
        let settings = crate::settings::Settings::load(root)
            .map_err(|e| RpcError::engine("config", format!("{e:#}")))?;
        Ok(json!({
            "file": file,
            "file_exists": file.is_file(),
            "settings": settings,
        }))
    }

    fn rpc_index(&mut self, params: Value) -> Reply {
        let p: IndexParams = rpc::params(params)?;
        let budget = p.embed_budget_ms.map(Duration::from_millis);
        to_value(
            self.engine(&p.root)?
                .index_focused_budgeted(p.full, budget, p.embed_budget_tokens, p.focus.as_deref())
                .map_err(engine_error)?,
        )
    }

    fn rpc_sync(&mut self, params: Value) -> Reply {
        let p: RootParams = rpc::params(params)?;
        keyed("pending", self.engine(&p.root)?.sync().map_err(engine_error))
    }

    #[cfg(feature = "postgres")]
    fn rpc_push(&mut self, params: Value) -> Reply {
        let p: PushParams = rpc::params(params)?;
        to_value(self.indexed(&p.root)?.push(p.dry_run).map_err(engine_error)?)
    }

    fn rpc_pull(&mut self, params: Value) -> Reply {
        let p: RootParams = rpc::params(params)?;
        to_value(self.indexed(&p.root)?.pull().map_err(engine_error)?)
    }

    fn rpc_status(&mut self, params: Value) -> Reply {
        let p: RootParams = rpc::params(params)?;
        to_value(self.engine(&p.root)?.status().map_err(engine_error)?)
    }

    fn rpc_purge(&mut self, params: Value) -> Reply {
        let p: RootParams = rpc::params(params)?;
        self.engine(&p.root)?;
        let path = Path::new(&p.root)
            .canonicalize()
            .map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let engine = self.engines.remove(&path).expect("opened above");
        engine.purge().map_err(engine_error)?;
        Ok(json!({ "removed": true }))
    }

    fn rpc_search(&mut self, params: Value) -> Reply {
        let p: SearchParams = rpc::params(params)?;
        let opts = SearchOpts {
            top_k: p.top_k,
            mode: p.mode,
            corpus: p.corpus,
            paths: p.paths,
            min_score: p.min_score,
            offset: p.offset,
            weights: None,
        };
        let mut timings = Timings::default();
        let hits = self
            .indexed(&p.root)?
            .search_timed(&p.query, &opts, &mut timings)
            .map_err(engine_error)?;
        Ok(json!({ "hits": hits, "timings": timings }))
    }

    fn rpc_context(&mut self, params: Value) -> Reply {
        let p: ContextParams = rpc::params(params)?;
        check_docs_share(p.docs_share)?;
        let engine = self.indexed(&p.root)?;
        let root = engine.root().to_path_buf();
        let share = self
            .asks
            .get(&root)
            .cloned()
            .unwrap_or_default()
            .docs_share(p.docs_share);
        let engine = self.engines.get(&root).expect("opened above");
        to_value(
            engine
                .context_shared(&p.query, p.token_budget, p.corpus, p.detail, share)
                .map_err(engine_error)?,
        )
    }

    fn rpc_chunks(&mut self, params: Value) -> Reply {
        let p: ChunksParams = rpc::params(params)?;
        keyed(
            "hits",
            self.indexed(&p.root)?.chunks(&p.ids).map_err(engine_error),
        )
    }

    fn rpc_rank(&mut self, params: Value) -> Reply {
        let p: RankParams = rpc::params(params)?;
        keyed(
            "ranked",
            self.engine(&p.root)?
                .rank(&p.documents, &p.query, p.top_k)
                .map_err(engine_error),
        )
    }

    fn rpc_grep(&mut self, params: Value) -> Reply {
        let p: GrepParams = rpc::params(params)?;
        keyed(
            "matches",
            self.engine(&p.root)?
                .grep(&p.pattern, &p.paths, p.max_results)
                .map_err(grep_error),
        )
    }

    fn rpc_ask(&mut self, params: Value) -> Reply {
        let p: AskParams = rpc::params(params)?;
        check_docs_share(p.request.docs_share)?;
        let gateway = loom_rag::llm::Client::new(None, None, String::new());
        let engine = self.indexed(&p.root)?;
        let root = engine.root().to_path_buf();
        let settings = self.asks.get(&root).cloned().unwrap_or_default();
        let engine = self.engines.get(&root).expect("opened above");
        to_value(crate::ask::ask(engine, &settings, &p.request, &gateway).map_err(engine_error)?)
    }

    fn rpc_symbols(&mut self, method: &str, params: Value) -> Reply {
        let p: SymbolParams = rpc::params(params)?;
        let engine = self.indexed(&p.root)?;
        let hits = if method == "symbols.definitions" {
            engine.definitions(&p.name)
        } else {
            engine.references(&p.name)
        }
        .map_err(engine_error)?;
        Ok(json!({ "hits": hits }))
    }

    fn rpc_history_index(&mut self, params: Value) -> Reply {
        let p: HistoryIndexParams = rpc::params(params)?;
        to_value(
            self.engine(&p.root)?
                .index_history(p.max_commits)
                .map_err(history_error)?,
        )
    }

    fn rpc_history_search(&mut self, params: Value) -> Reply {
        let p: HistorySearchParams = rpc::params(params)?;
        let hits = self
            .engine(&p.root)?
            .search_history(&p.query, p.top_k)
            .map_err(history_error)?;
        Ok(json!({ "hits": hits }))
    }

    fn rpc_communities(&mut self, params: Value) -> Reply {
        let p: GraphListParams = rpc::params(params)?;
        keyed(
            "communities",
            self.indexed(&p.root)?
                .graph_communities(p.limit, p.min_size)
                .map_err(engine_error),
        )
    }

    fn rpc_god_nodes(&mut self, params: Value) -> Reply {
        let p: GraphListParams = rpc::params(params)?;
        keyed(
            "nodes",
            self.indexed(&p.root)?
                .graph_god_nodes(p.min_in_degree, p.limit)
                .map_err(engine_error),
        )
    }

    fn rpc_graph_path(&mut self, params: Value) -> Reply {
        let p: PathParams = rpc::params(params)?;
        to_value(
            self.indexed(&p.root)?
                .graph_path(&p.from, &p.to, &p.kinds, p.max_hops)
                .map_err(engine_error)?,
        )
    }

    fn rpc_explain(&mut self, params: Value) -> Reply {
        let p: ExplainParams = rpc::params(params)?;
        keyed(
            "matches",
            self.indexed(&p.root)?
                .graph_explain(&p.target)
                .map_err(engine_error),
        )
    }
}

/// `grep` reports a bad pattern as invalid params and every other failure as an engine error.
fn grep_error(e: Error) -> RpcError {
    match e {
        Error::Other(e) => RpcError::invalid_params(e.to_string()),
        e => engine_error(e),
    }
}

fn quality_method(method: &str, params: Value) -> Reply {
    match method {
        "quality.loc" => quality::loc(params),
        "quality.clones" => quality::clones(params),
        "quality.complexity" => quality::complexity(params),
        "quality.structure" => quality::structure(params),
        "quality.rstest" => quality::rstest(params),
        _ => Err(unknown(method)),
    }
}
