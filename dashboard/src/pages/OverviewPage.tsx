/**
 * Overview: the starting point (dashboard-ux.md, "Overview is the starting point"; audit A-02, A-16). Getting started
 * and Connect your app at the top, then server health and recent activity.
 */
import { useState } from "react";
import { useSession } from "../auth/session";
import { PageHeader } from "../components/PageHeader";
import { isStudioDisabled, studio } from "../studios";
import { useSharedFeatures } from "./setup/features";
import { ConnectApp } from "./overview/ConnectApp";
import { GettingStarted } from "./overview/GettingStarted";
import { RecentActivity } from "./overview/RecentActivity";
import { ServerHealth } from "./overview/ServerHealth";
import { gettingStartedSteps } from "./overview/model";
import { newestCollection, useProjectState } from "./overview/state";

export default function OverviewPage() {
  const { admin } = useSession();
  const features = useSharedFeatures()?.snapshot?.features;
  const project = useProjectState();
  const collections = project.data?.collections;
  const newest = collections?.state === "ok" ? newestCollection(collections.value) : undefined;
  const disabled = (path: string) => isStudioDisabled(studio(path), features);
  const [showDone, setShowDone] = useState(false);
  // A finished checklist folds into one line; Connect your app then takes the full width instead of half.
  const folded = !showDone && !!project.data && gettingStartedSteps(project.data, { disabled }).every((s) => s.done);
  // Side by side only on very wide screens: below that each card gets the full width, so the snippet is not clipped.
  return (
    <div className="max-w-6xl space-y-8">
      <PageHeader studio="" />
      <div className={`grid items-start gap-4 ${folded ? "" : "2xl:grid-cols-[minmax(0,5fr)_minmax(0,7fr)]"}`}>
        <GettingStarted project={project.data} adminEmail={admin?.email} disabled={disabled} showDone={showDone} onShowDoneChange={setShowDone} />
        <ConnectApp collection={newest?.name} />
      </div>
      <ServerHealth />
      <RecentActivity adminEmail={admin?.email} adminId={admin?.id} />
    </div>
  );
}
