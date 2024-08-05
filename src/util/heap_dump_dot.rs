use crate::util::{ObjectReference, VMWorkerThread};
use crate::vm::{RootsWorkFactory, Scanning, SlotVisitor, VMBinding};
use core::marker::Send;
use crossbeam::queue::SegQueue;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub fn check_gc<Pred, VM>(pred: Pred, tls: VMWorkerThread)
where
    VM: VMBinding + ConservativeScanning + std::fmt::Debug,
    Pred: ValidityPredicate<VM> + Send + Clone + 'static,
{
    let mut checker: SanityChecker<Pred, VM> = SanityChecker::new(pred, tls);
    checker.check_roots();
    let msg = checker.worker.errors.lock().expect("uh oh");
    if !msg.is_empty() {
        for message in msg.iter() {
            println!("{:?}", message);
        }
        panic!("things went poorly");
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum Error<VM: VMBinding> {
    BadEdge(<VM as VMBinding>::VMSlot, String),
    BadNode(ObjectReference, String),
}

pub trait ConservativeScanning: VMBinding {
    /// Scan an object assuming any weak references are strong.
    fn conservatively_scan_object<EV: SlotVisitor<<Self as VMBinding>::VMSlot>>(
        _tls: VMWorkerThread,
        obj: ObjectReference,
        ev: &mut EV,
    );
}

pub trait ValidityPredicate<VM: VMBinding> {
    fn is_valid_node(&self, node: ObjectReference) -> Result<(), String>;
    fn is_valid_slot(&self, slot: <VM as VMBinding>::VMSlot) -> Result<(), String>;
}

////////////////////////////////////////////////////
// WorkFactory
////////////////////////////////////////////////////
struct WorkFactory<Pred: ValidityPredicate<VM> + Clone, VM: VMBinding> {
    errors: Arc<Mutex<Vec<Error<VM>>>>,
    work_list: Arc<SegQueue<ObjectReference>>,
    pred: Pred,
}

impl<Pred, VM> Clone for WorkFactory<Pred, VM>
where
    VM: VMBinding,
    Pred: ValidityPredicate<VM> + Send + Clone,
{
    fn clone(&self) -> Self {
        WorkFactory {
            errors: self.errors.clone(),
            work_list: self.work_list.clone(),
            pred: self.pred.clone(),
        }
    }
}

impl<Pred, VM> WorkFactory<Pred, VM>
where
    VM: VMBinding,
    Pred: ValidityPredicate<VM> + Send + Clone,
{
    fn push_error(&mut self, error: Error<VM>) {
        self.errors.lock().expect("Failed to lock").push(error);
    }

    fn push_slot(&mut self, slot: <VM as VMBinding>::VMSlot) {
        use crate::vm::slot::Slot;
        match self.pred.is_valid_slot(slot) {
            Ok(()) => {
                let node = slot.load().expect("invalid");
                self.work_list.push(node);
            }
            Err(error) => self
                .errors
                .lock()
                .expect("failed to lock")
                .push(Error::BadEdge(slot, error)),
        }
    }

    fn push_nodes(&mut self, nodes: Vec<ObjectReference>) {
        nodes.into_iter().for_each(|node| self.work_list.push(node));
    }
}

impl<Pred, VM> RootsWorkFactory<<VM as VMBinding>::VMSlot> for WorkFactory<Pred, VM>
where
    VM: VMBinding,
    Pred: ValidityPredicate<VM> + Send + Clone + 'static,
{
    fn create_process_pinning_roots_work(&mut self, nodes: Vec<ObjectReference>) {
        self.push_nodes(nodes);
    }

    fn create_process_tpinning_roots_work(&mut self, nodes: Vec<ObjectReference>) {
        self.push_nodes(nodes);
    }

    fn create_process_roots_work(&mut self, slots: Vec<<VM as VMBinding>::VMSlot>) {
        slots.into_iter().for_each(|slot| self.push_slot(slot))
    }
}

////////////////////////////////////////////////////
// Sanity Checker
////////////////////////////////////////////////////

pub struct SanityChecker<
    Pred: Send + ValidityPredicate<VM> + Clone,
    VM: VMBinding + ConservativeScanning,
> {
    tls: VMWorkerThread,
    visited: HashSet<ObjectReference>,
    worker: WorkFactory<Pred, VM>,
    pred: Pred,
}

impl<Pred, VM> SlotVisitor<<VM as VMBinding>::VMSlot> for SanityChecker<Pred, VM>
where
    VM: VMBinding + ConservativeScanning,
    Pred: ValidityPredicate<VM> + Send + Clone,
{
    fn visit_slot(&mut self, slot: <VM as VMBinding>::VMSlot) {
        self.worker.push_slot(slot);
    }
}

impl<Pred, VM> SanityChecker<Pred, VM>
where
    VM: VMBinding + ConservativeScanning,
    Pred: ValidityPredicate<VM> + Send + Clone + 'static,
{
    pub fn new(pred: Pred, tls: VMWorkerThread) -> Self {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let work_list = Arc::new(SegQueue::new());
        let worker = WorkFactory {
            errors: errors,
            work_list: work_list,
            pred: pred.clone(),
        };
        SanityChecker {
            tls,
            visited: HashSet::new(),
            worker,
            pred,
        }
    }

    pub fn check_roots(&mut self) {
        <VM::VMScanning as Scanning<VM>>::scan_vm_specific_roots(self.tls, self.worker.clone());

        loop {
            match self.worker.work_list.pop() {
                None => {
                    break;
                }
                Some(node) => self.check_node(node),
            }
        }
    }

    fn check_node(&mut self, node: ObjectReference) {
        if !self.visited.contains(&node) {
            self.visited.insert(node);
            match self.pred.is_valid_node(node) {
                Ok(()) => {
                    <VM as ConservativeScanning>::conservatively_scan_object(self.tls, node, self)
                }
                Err(error) => self.worker.push_error(Error::BadNode(node, error)),
            }
        }
    }
}

////////////////////////////////////////////////////
// Heap graph dump
////////////////////////////////////////////////////

pub mod graph {
    use super::{ConservativeScanning, ValidityPredicate};
    use crate::vm::slot::Slot;
    use crate::vm::Scanning;
    use crate::{
        util::{ObjectReference, VMWorkerThread},
        vm::{SlotVisitor, VMBinding},
    };
    use crossbeam::queue::SegQueue;
    use std::collections::HashSet;
    use std::io::Write;
    use std::marker::PhantomData;
    use std::sync::Arc;

    pub fn dump_dot<Pred, VM>(
        pred: Pred,
        tls: VMWorkerThread,
        path: &std::path::Path,
    ) -> Result<(), std::io::Error>
    where
        Pred: Send + ValidityPredicate<VM> + Clone,
        VM: VMBinding + ConservativeScanning + NodeAttrs,
    {
        let f = std::fs::File::create(path)?;
        let out = DotOutput::new(f);
        let mut dumper: HeapGraphDumper<Pred, VM, DotOutput> = HeapGraphDumper::new(pred, tls, out);
        dumper.visit_roots();
        Ok(())
    }

    pub struct NodeId(String);

    pub enum NodeAttr {
        String(String, String),
        Number(String, usize),
        Flag(String),
    }

    pub trait GraphOutput {
        fn add_slot(&mut self, src: &NodeId, dst: &NodeId) -> Result<(), std::io::Error>;
        fn add_node(
            &mut self,
            node_id: &NodeId,
            attrs: &Vec<NodeAttr>,
        ) -> Result<(), std::io::Error>;
    }

    pub struct DotOutput {
        output: std::fs::File,
    }

    impl DotOutput {
        pub fn new(mut file: std::fs::File) -> Self {
            write!(file, "digraph heap {{\n").unwrap();
            DotOutput { output: file }
        }
    }

    impl Drop for DotOutput {
        fn drop(&mut self) {
            write!(self.output, "}}\n").unwrap();
        }
    }

    impl GraphOutput for DotOutput {
        fn add_slot(&mut self, src: &NodeId, dst: &NodeId) -> Result<(), std::io::Error> {
            write!(self.output, "  {} -> {};\n", src.0, dst.0)
        }

        fn add_node(
            &mut self,
            node_id: &NodeId,
            attrs: &Vec<NodeAttr>,
        ) -> Result<(), std::io::Error> {
            write!(&self.output, "  {} [", node_id.0)?;
            for attr in attrs.into_iter() {
                match attr {
                    NodeAttr::Flag(key) => write!(self.output, "{}=yes ", key)?,
                    NodeAttr::String(key, value) => write!(self.output, "{}=\"{}\" ", key, value)?,
                    NodeAttr::Number(key, value) => write!(self.output, "{}={} ", key, value)?,
                }
            }
            write!(self.output, "];\n")
        }
    }

    pub trait NodeAttrs {
        fn node_attrs(obj: ObjectReference) -> Vec<NodeAttr>;
    }

    struct RootSet<VM: VMBinding> {
        work_list: Arc<SegQueue<ObjectReference>>,
        _phantom: PhantomData<VM>,
    }

    impl<VM: VMBinding> Clone for RootSet<VM> {
        fn clone(&self) -> Self {
            RootSet {
                work_list: self.work_list.clone(),
                _phantom: PhantomData,
            }
        }
    }

    impl<VM> RootSet<VM>
    where
        VM: VMBinding,
    {
        pub fn new() -> Self {
            RootSet {
                work_list: Arc::new(SegQueue::new()),
                _phantom: PhantomData,
            }
        }

        pub fn push_slot(&self, slot: <VM as VMBinding>::VMSlot) {
            let obj = slot.load();
            match obj {
                Some(s) => self.push_node(s),
                None => log::warn!("Invalid slot {:?}", slot),
            }
        }

        pub fn push_node(&self, node: ObjectReference) {
            self.work_list.push(node);
        }

        pub fn push_nodes(&self, nodes: Vec<ObjectReference>) {
            for node in nodes.iter() {
                self.push_node(*node);
            }
        }
    }

    impl<VM> crate::vm::RootsWorkFactory<<VM as VMBinding>::VMSlot> for RootSet<VM>
    where
        VM: VMBinding,
    {
        fn create_process_roots_work(&mut self, slots: Vec<<VM as VMBinding>::VMSlot>) {
            slots.into_iter().for_each(|slot| self.push_slot(slot))
        }

        fn create_process_pinning_roots_work(&mut self, nodes: Vec<ObjectReference>) {
            self.push_nodes(nodes);
        }

        fn create_process_tpinning_roots_work(&mut self, nodes: Vec<ObjectReference>) {
            self.push_nodes(nodes);
        }
    }

    pub struct HeapGraphDumper<Pred, VM, Output>
    where
        Pred: Send + ValidityPredicate<VM> + Clone,
        VM: VMBinding + ConservativeScanning,
        Output: GraphOutput,
    {
        tls: VMWorkerThread,
        visited: HashSet<ObjectReference>,
        root_set: RootSet<VM>,
        current_source: NodeId,
        work_list: SegQueue<ObjectReference>,
        output: Output,
        pred: Pred,
    }

    impl<Pred, VM, Output> SlotVisitor<<VM as VMBinding>::VMSlot> for HeapGraphDumper<Pred, VM, Output>
    where
        VM: VMBinding + ConservativeScanning + NodeAttrs,
        Pred: ValidityPredicate<VM> + Send + Clone,
        Output: GraphOutput,
    {
        fn visit_slot(&mut self, slot: <VM as VMBinding>::VMSlot) {
            let src = &self.current_source;
            let dst = slot.load().expect("invalid");
            self.output.add_slot(&src, &self.to_node_id(dst)).unwrap();
            self.work_list.push(dst);
        }
    }

    impl<Pred, VM, Output> HeapGraphDumper<Pred, VM, Output>
    where
        VM: VMBinding + ConservativeScanning + NodeAttrs,
        Pred: ValidityPredicate<VM> + Send + Clone,
        Output: GraphOutput,
    {
        pub fn new(pred: Pred, tls: VMWorkerThread, output: Output) -> Self {
            HeapGraphDumper {
                tls,
                visited: HashSet::new(),
                root_set: RootSet::new(),
                current_source: NodeId(String::from("unknown")),
                work_list: SegQueue::new(),
                output: output,
                pred: pred,
            }
        }

        pub fn to_node_id(&self, obj: ObjectReference) -> NodeId {
            NodeId(format!("\"{:p}\"", obj.to_raw_address().to_ptr::<usize>()))
        }

        pub fn visit_roots(&mut self) {
            <VM::VMScanning as Scanning<VM>>::scan_vm_specific_roots(
                self.tls,
                self.root_set.clone(),
            );

            loop {
                match self.root_set.work_list.pop() {
                    None => (),
                    Some(node) => {
                        let node_id = self.to_node_id(node);
                        self.visit_node(node);
                        self.output
                            .add_slot(&NodeId(String::from("roots")), &node_id)
                            .unwrap();
                    }
                }

                match self.work_list.pop() {
                    None => {
                        break;
                    }
                    Some(node) => self.visit_node(node),
                }
            }
        }

        fn visit_node(&mut self, node: ObjectReference) {
            if !self.visited.contains(&node) {
                self.visited.insert(node);
                let node_id = self.to_node_id(node);
                self.current_source = self.to_node_id(node);
                match self.pred.is_valid_node(node) {
                    Ok(()) => {
                        let attrs = <VM as NodeAttrs>::node_attrs(node);
                        self.output.add_node(&node_id, &attrs).unwrap();
                        <VM as ConservativeScanning>::conservatively_scan_object(
                            self.tls, node, self,
                        );
                    }
                    Err(error) => {
                        let attrs = vec![NodeAttr::String("error".to_string(), error.to_string())];
                        self.output.add_node(&node_id, &attrs).unwrap();
                    }
                }
                self.current_source = NodeId(String::from("unknown"));
            }
        }
    }
}
