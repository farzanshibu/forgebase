import { Link, NavLink, Route, Routes, useParams } from "react-router-dom";
import { bucketUsage, getBucket } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import AclEditor from "./AclEditor";
import BucketSettings from "./BucketSettings";
import ObjectBrowser from "./ObjectBrowser";
import SignedUrls from "./SignedUrls";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`;

export default function BucketView() {
  const { bucket = "" } = useParams();
  const b = useLoad((s) => getBucket(bucket, s), `bucket:${bucket}`);
  const usage = useLoad((s) => bucketUsage(bucket, s), `bucket-usage:${bucket}`);
  const base = `/storage/b/${encodeURIComponent(bucket)}`;
  return (
    <div className="space-y-4">
      <div className="flex items-baseline gap-3">
        <Link className="text-sm text-primary hover:underline" to="/storage">All buckets</Link>
        <h2 className="text-xl font-semibold">{bucket}</h2>
        {b.data && <span className="text-sm text-muted-foreground">{b.data.visibility} · versioning {b.data.versioning ? "on" : "off"}</span>}
        {usage.data && <span aria-label="Bucket usage" className="text-sm text-muted-foreground">{usage.data.objects} objects · {formatBytes(usage.data.logical_bytes)}</span>}
      </div>
      <ErrorView error={b.error} />
      {b.loading && !b.error && <Loading what="Loading bucket" />}
      {b.data && (
        <>
          <nav aria-label="Bucket sections" className="flex flex-wrap gap-1 border-b border-border">
            <NavLink to={base} end className={tab}>Objects</NavLink>
            <NavLink to={`${base}/settings`} className={tab}>Policy</NavLink>
            <NavLink to={`${base}/access`} className={tab}>Access (ACL)</NavLink>
            <NavLink to={`${base}/share`} className={tab}>Signed URLs</NavLink>
          </nav>
          <Routes>
            <Route index element={<ObjectBrowser bucket={b.data} />} />
            <Route path="settings" element={<BucketSettings bucket={b.data} onSaved={b.reload} />} />
            <Route path="access" element={<AclEditor bucket={b.data} />} />
            <Route path="share" element={<SignedUrls bucket={b.data} />} />
            <Route path="*" element={<p className="text-muted-foreground">No such page in this bucket.</p>} />
          </Routes>
        </>
      )}
    </div>
  );
}
