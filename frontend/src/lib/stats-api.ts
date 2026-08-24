import { api } from '@/lib/api-client';
import type { StatsResponse } from '@/types';

export async function fetchStats(days: number): Promise<StatsResponse> {
  return api<StatsResponse>(`/messages/stats?days=${days}`);
}
