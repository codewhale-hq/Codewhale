import { NextResponse } from "next/server";
import { getFacts } from "@/lib/facts";
import { getReleaseUpdates } from "@/lib/release-updates";

export const dynamic = "force-dynamic";

export async function GET() {
  const facts = await getFacts();
  const snapshot = await getReleaseUpdates(facts.latestPublishedRelease);
  return NextResponse.json(snapshot, {
    headers: { "Cache-Control": "public, max-age=0, must-revalidate" },
  });
}
