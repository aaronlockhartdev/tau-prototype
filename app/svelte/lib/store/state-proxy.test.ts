import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import BagShapes from './BagShapes.svelte';

// Why `store.entryOpen` is a Record and not a Map (#FE9, 2026-11-06): a Map
// inside `$state` is not deep-proxied, so `.set` on it fires no reader. The
// card-expansion map is read by mounted cards after a toggle, so it must ride
// the representation the runtime does track. Pinned here because the pin is
// Svelte's behaviour, not ours: a future Svelte that proxies Maps lets the
// store switch representation, and this test says so in its red.
describe('$state deep proxying (Svelte 5.57.2)', () => {
  it('tracks a record key add, so a record bag is the reactive expansion shape', async () => {
    render(BagShapes);
    await tick();
    expect(screen.getByText('rec:no')).toBeInTheDocument();
    screen.getByRole('button', { name: /bump record/ }).click();
    await tick();
    expect(screen.getByText('rec:yes')).toBeInTheDocument();
  });

  it('does not track a Map .set, so a Map bag cannot be the expansion shape', async () => {
    render(BagShapes);
    await tick();
    expect(screen.getByText('map:no')).toBeInTheDocument();
    screen.getByRole('button', { name: /bump map/ }).click();
    await tick();
    expect(screen.getByText('map:no')).toBeInTheDocument();
  });
});
