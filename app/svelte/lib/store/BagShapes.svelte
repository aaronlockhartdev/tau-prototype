<script lang="ts">
  let bag = $state({ open: new Map<string, boolean>(), keyed: {} as Record<string, boolean> });
  const mapShown = $derived(bag.open.get('k') ? 'yes' : 'no');
  const recShown = $derived(bag.keyed['k'] ? 'yes' : 'no');
</script>

<button onclick={() => bag.keyed['k']}>bump record</button>
<button onclick={() => bag.open.set('k', true)}>bump map</button>
<span>rec:{recShown}</span>
<span>map:{mapShown}</span>
