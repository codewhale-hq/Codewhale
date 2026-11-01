// codewhale-native-preset-v1 {"id":"b","trust":"user","is_default":false,"name":"Preset Beta","description":"Second caller only","order":2}
import { mountReviewedPreset } from '@codewhale/dsh-composition';
import data from './b.json' with { type: 'json' };
export async function apply(ctx) { await mountReviewedPreset(ctx, new URL('../../source/', import.meta.url).href, data.composition, data.catalog, data.selected); }
