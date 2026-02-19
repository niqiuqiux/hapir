use std::collections::{HashMap, HashSet};

/// Maps RPC method names to WebSocket connection IDs,
/// and tracks which methods belong to which connection.
pub struct RpcRegistry {
    method_to_conn: HashMap<String, String>,
    conn_to_methods: HashMap<String, HashSet<String>>,
}

impl RpcRegistry {
    pub fn new() -> Self {
        Self {
            method_to_conn: HashMap::new(),
            conn_to_methods: HashMap::new(),
        }
    }

    pub fn register(&mut self, conn_id: &str, method: &str) {
        if method.is_empty() {
            return;
        }
        self.method_to_conn.insert(method.to_string(), conn_id.to_string());
        self.conn_to_methods
            .entry(conn_id.to_string())
            .or_default()
            .insert(method.to_string());
    }

    pub fn unregister(&mut self, conn_id: &str, method: &str) {
        if let Some(cid) = self.method_to_conn.get(method)
            && cid == conn_id
        {
            self.method_to_conn.remove(method);
        }
        if let Some(methods) = self.conn_to_methods.get_mut(conn_id) {
            methods.remove(method);
            if methods.is_empty() {
                self.conn_to_methods.remove(conn_id);
            }
        }
    }

    pub fn unregister_all(&mut self, conn_id: &str) {
        if let Some(methods) = self.conn_to_methods.remove(conn_id) {
            for method in methods {
                if let Some(cid) = self.method_to_conn.get(&method)
                    && cid == conn_id
                {
                    self.method_to_conn.remove(&method);
                }
            }
        }
    }

    pub fn get_conn_id_for_method(&self, method: &str) -> Option<&str> {
        self.method_to_conn.get(method).map(|s| s.as_str())
    }

    /// Return all registered method names (for diagnostics).
    pub fn all_methods(&self) -> Vec<&str> {
        self.method_to_conn.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut reg = RpcRegistry::new();
        reg.register("conn1", "session1:permission");
        assert_eq!(reg.get_conn_id_for_method("session1:permission"), Some("conn1"));
    }

    #[test]
    fn unregister_single() {
        let mut reg = RpcRegistry::new();
        reg.register("conn1", "method1");
        reg.register("conn1", "method2");
        reg.unregister("conn1", "method1");
        assert_eq!(reg.get_conn_id_for_method("method1"), None);
        assert_eq!(reg.get_conn_id_for_method("method2"), Some("conn1"));
    }

    #[test]
    fn unregister_all() {
        let mut reg = RpcRegistry::new();
        reg.register("conn1", "m1");
        reg.register("conn1", "m2");
        reg.register("conn2", "m3");
        reg.unregister_all("conn1");
        assert_eq!(reg.get_conn_id_for_method("m1"), None);
        assert_eq!(reg.get_conn_id_for_method("m2"), None);
        assert_eq!(reg.get_conn_id_for_method("m3"), Some("conn2"));
    }

    #[test]
    fn wrong_conn_unregister_noop() {
        let mut reg = RpcRegistry::new();
        reg.register("conn1", "method1");
        reg.unregister("conn2", "method1");
        assert_eq!(reg.get_conn_id_for_method("method1"), Some("conn1"));
    }
}
