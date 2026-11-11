import { Alert } from "panelui-web/alert";
import { ErrorPanel } from "../../components/ErrorPanel";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonCards } from "../../components/Skeleton";
import FeaturesForm from "./FeaturesForm";
import { useSharedFeatures } from "./features";

/** System, "Features and settings": switch features on or off and edit their settings, on one page with one Save. */
export default function FeaturesPage() {
  const data = useSharedFeatures();
  const header = <PageHeader studio="system/features" />;
  if (!data || data.loading) return <div>{header}<div className="mt-6"><SkeletonCards count={6} label="Loading features" /></div></div>;
  if (!data.snapshot) {
    return (
      <div>
        {header}
        <div className="mt-6">
          {data.error ? (
            <ErrorPanel error={data.error} onRetry={data.reload} />
          ) : (
            <Alert tone="warning" variant="plain">
              <Alert.Icon />
              <Alert.Title>The feature list is not available</Alert.Title>
            </Alert>
          )}
        </div>
      </div>
    );
  }
  return (
    <div className="max-w-5xl">
      {header}
      <div className="mt-6">
        <FeaturesForm snapshot={data.snapshot} apply={data.apply} />
      </div>
    </div>
  );
}
