import { capabilityViews, runtimeCapabilityContext } from "@/lib/capabilities";
import { BUILD_FACTS } from "@/lib/facts";
import { FEATURE_CATALOGUE } from "@/lib/features";

export const dynamic = "force-static";

export function GET() {
  const context = runtimeCapabilityContext(BUILD_FACTS);
  return Response.json({
    ...FEATURE_CATALOGUE,
    publishedRelease: context.release?.tag ?? null,
    features: capabilityViews(context).map(({ feature, availability, name, summary }) => ({ ...feature, name, summary, availability })),
  }, {
    headers: { "cache-control": "public, max-age=3600" },
  });
}
