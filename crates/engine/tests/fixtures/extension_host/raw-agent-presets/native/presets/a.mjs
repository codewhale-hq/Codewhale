// codewhale-native-preset-v1 {"id":"a","trust":"user","is_default":true,"name":"Preset Alpha","description":"First caller only","order":1}
import { mountReviewedPreset } from '@codewhale/dsh-composition';
import data from './a.json' with { type: 'json' };
export async function apply(ctx) { await mountReviewedPreset(ctx, new URL('../../source/', import.meta.url).href, data.composition, data.catalog, data.selected); }
