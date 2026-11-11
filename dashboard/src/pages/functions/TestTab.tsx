import { listFunctionVersions } from "../../api";
import type { FunctionInfo } from "../../api";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonLines } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import TestPanel from "./TestPanel";

export default function TestTab({ info }: { info: FunctionInfo }) {
  const versions = useLoad((s) => listFunctionVersions(info.name, s), `test-versions:${info.name}:${info.latest_version}`);
  if (versions.error) return <ErrorPanel error={versions.error} onRetry={versions.reload} />;
  if (!versions.data) return <SkeletonLines lines={4} label="Loading versions" />;
  return <TestPanel info={info} versions={versions.data} />;
}
