<script lang="ts">
  type Bag = { open: Map<string, boolean>; keyed: Record<string, boolean> };
  let bag = $state<Bag>({ open: new Map(), keyed: {} });
  const mapShown = $derived(bag.open.get('k') ? 'yes' : 'no');
  const recShown = $derived(bag.keyed['k'] ? 'yes' : 'no');
</script>

<button onclick={() => { bag.keyed['k'] = true; }}>bump record</button>
<button onclick={() => { bag.open.set('k', true); }}>bump map</button>
<span>rec:{recShown}</span>
<span>map:{mapShown}</span>
