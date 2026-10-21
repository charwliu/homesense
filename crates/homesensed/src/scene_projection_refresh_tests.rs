mod tests {
    use super::super::TaskGeneration;
    use crate::scene_projection_refresh::Queue;
    use homesense_core::queue::Epochs;
    fn generation() -> TaskGeneration {
        TaskGeneration { boot: "boot".into(), epochs: Epochs { transport: 1, configuration: 1, control: 1 } }
    }
    #[test]
    fn changed_tail_gets_next_slot_without_starving_background() {
        let mut queue = Queue::default(); let g = generation(); let mut cursor = 0;
        let names = (0..80).map(|n| n.to_string()).collect::<Vec<_>>();
        let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        for expected in 0..79 {
            queue.request(&g, "79");
            assert_eq!(queue.batch(&g, &refs, &mut cursor), [79, expected]);
        }
        queue.request(&g, "79");
        assert_eq!(queue.batch(&g, &refs, &mut cursor), [79, 0]);
    }
    #[test]
    fn bounded_fifo_deduplicates_and_discards_old_scope_and_missing_groups() {
        let mut queue = Queue::default(); let mut g = generation(); let mut cursor = 0;
        queue.request(&g, "missing"); queue.request(&g, "c"); queue.request(&g, "c"); queue.request(&g, "d");
        assert_eq!(queue.batch(&g, &["a", "b", "c", "d"], &mut cursor), [2, 0]);
        assert_eq!(queue.batch(&g, &["a", "b", "c", "d"], &mut cursor), [3, 1]);
        for n in 0..300 { queue.request(&g, &n.to_string()); }
        let names = (0..300).map(|n| n.to_string()).collect::<Vec<_>>();
        let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        for expected in 0..256 { cursor = 299; assert_eq!(queue.batch(&g, &refs, &mut cursor)[0], expected); }
        cursor = 299;
        assert_eq!(queue.batch(&g, &refs, &mut cursor), [299, 0], "overflow requests use background fairness");
        g.epochs.transport += 1;
        assert_eq!(queue.batch(&g, &["a", "b"], &mut cursor).len(), 2);
        cursor = 0; assert_eq!(queue.batch(&g, &["a", "b"], &mut cursor), [0, 1]);
        queue.request(&g, "a"); queue.completed(&g, "a"); cursor = 0; assert_eq!(queue.batch(&g, &["a", "b"], &mut cursor), [0, 1]);
        assert!(queue.batch(&g, &[], &mut cursor).is_empty());
    }
}
