import { describe, expect, it, vi, afterEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { renderHook } from '@testing-library/react';
import { StellarAgent } from '@stellaragent/core';
import { StellarAgentProvider, useStellarAgent } from '../StellarAgentProvider.js';
import { createMockAgent } from '../test/mockAgent.js';

afterEach(() => {
  vi.restoreAllMocks();
});

function StatusProbe() {
  const { agent, status, error } = useStellarAgent();
  return (
    <div>
      <span data-testid="status">{status}</span>
      <span data-testid="address">{agent?.address ?? 'none'}</span>
      <span data-testid="error">{error?.message ?? 'none'}</span>
    </div>
  );
}

describe('StellarAgentProvider', () => {
  it('is ready immediately when an agent is injected', () => {
    const mockAgent = createMockAgent();
    render(
      <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
        <StatusProbe />
      </StellarAgentProvider>,
    );

    expect(screen.getByTestId('status').textContent).toBe('ready');
    expect(screen.getByTestId('address').textContent).toBe(mockAgent.address);
  });

  it('transitions idle -> loading -> ready when constructing from config', async () => {
    const mockAgent = createMockAgent();
    let resolveCreate!: (agent: StellarAgent) => void;
    vi.spyOn(StellarAgent, 'create').mockReturnValue(
      new Promise((resolve) => {
        resolveCreate = resolve;
      }),
    );

    render(
      <StellarAgentProvider config={{ network: 'local' }}>
        <StatusProbe />
      </StellarAgentProvider>,
    );

    expect(screen.getByTestId('status').textContent).toBe('loading');

    resolveCreate(mockAgent);

    await waitFor(() => expect(screen.getByTestId('status').textContent).toBe('ready'));
    expect(screen.getByTestId('address').textContent).toBe(mockAgent.address);
  });

  it('transitions to error when StellarAgent.create rejects', async () => {
    vi.spyOn(StellarAgent, 'create').mockRejectedValue(new Error('friendbot unreachable'));

    render(
      <StellarAgentProvider config={{ network: 'testnet' }}>
        <StatusProbe />
      </StellarAgentProvider>,
    );

    await waitFor(() => expect(screen.getByTestId('status').textContent).toBe('error'));
    expect(screen.getByTestId('error').textContent).toBe('friendbot unreachable');
    expect(screen.getByTestId('address').textContent).toBe('none');
  });

  it('useStellarAgent throws outside a provider', () => {
    // Swallow the expected React error-boundary console noise for this case.
    const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => renderHook(() => useStellarAgent())).toThrow(
      'useStellarAgent must be used within a <StellarAgentProvider>',
    );
    consoleSpy.mockRestore();
  });
});
