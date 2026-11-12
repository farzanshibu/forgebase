import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { bucketUsage, getBucket } from "../../api";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import ResourceRules from "../../components/ResourceRules";
import AclEditor from "./AclEditor";
import BucketSettings from "./BucketSettings";
import ObjectBrowser from "./ObjectBrowser";
import SignedUrls from "./SignedUrls";
import { EmptyState } from "../../components/EmptyState";
import { Facts } from "../../components/Facts";
import { VisibilityBadge } from "./parts";
import { bucketScope } from "./util";

export default function BucketView() {
  const { bucket = "" } = useParams();
  const b = useLoad((s) => getBucket(bucket, s), `bucket:${bucket}`);
  const usage = useLoad((s) => bucketUsage(bucket, s), `bucket-usage:${bucket}`);
  const base = `/storage/b/${encodeURIComponent(bucket)}`;
  return (
    <div className="space-y-5">
      <PageHeader
        title={bucket}
        description="Bucket"
        docs="docs/adr/0018-object-storage.md"
        secondary={<UseFromCodeButton kind="bucket" name={bucket} />}
      >
        {b.data && (
          <Facts
            className="mt-3"
            label="Bucket facts"
            items={[
              ["Visibility", <VisibilityBadge value={b.data.visibility} />],
              ["Versioning", b.data.versioning ? "On" : "Off"],
              ["Scope", <span title={bucketScope(b.data).text}>{b.data.tenant ? `Tenant ${b.data.tenant}` : "Instance"}</span>],
              ["Objects", <span className="tabular-nums">{usage.data ? usage.data.objects : "…"}</span>],
              ["Size", <span className="tabular-nums">{usage.data ? formatBytes(usage.data.logical_bytes) : "…"}</span>],
              ["Max object", b.data.max_object_bytes ? formatBytes(b.data.max_object_bytes) : "No limit"],
            ]}
          />
        )}
      </PageHeader>
      <ErrorView error={b.error} onRetry={b.reload} />
      {b.loading && !b.error && <SkeletonLines lines={4} label="Loading bucket" />}
      {b.data && (
        <>
          <RouteTabs className="mt-1" label="Bucket sections" tabs={[{ to: base, label: "Objects", end: true }, { to: `${base}/settings`, label: "Policy" }, { to: `${base}/access`, label: "Access (ACL)" }, { to: `${base}/rules`, label: "Rules" }, { to: `${base}/share`, label: "Signed URLs" }]} />
          <Routes>
            <Route index element={<ObjectBrowser bucket={b.data} />} />
            <Route path="settings" element={<BucketSettings bucket={b.data} onSaved={b.reload} />} />
            <Route path="access" element={<AclEditor bucket={b.data} />} />
            <Route path="rules" element={<BucketRules name={b.data.name} />} />
            <Route path="share" element={<SignedUrls bucket={b.data} />} />
            <Route path="*" element={<EmptyState compact title="No such page in this bucket" why="The link may be old. Pick a tab above." />} />
          </Routes>
        </>
      )}
    </div>
  );
}

/** Object rules of the bucket (FB-140): who may list, read, create, update or delete objects, by owner, folder, size or type. */
function BucketRules({ name }: { name: string }) {
  return (
    <div className="max-w-5xl space-y-3">
      <p className="text-sm text-muted-foreground">
        Rules refine the access control list per object. An expression can test <code className="font-mono">key</code>, <code className="font-mono">owner</code>, <code className="font-mono">created_by</code>, <code className="font-mono">size</code>, <code className="font-mono">content_type</code> and <code className="font-mono">folders.0</code> (the first folder). An object a rule hides answers 404.
      </p>
      <ResourceRules kind="bucket" name={name} noun="bucket" />
    </div>
  );
}
