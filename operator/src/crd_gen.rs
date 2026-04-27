use std::path::Path;

use kube::CustomResourceExt;

use fluidbg_operator::crd::blue_green::BlueGreenDeployment;
use fluidbg_operator::crd::inception_plugin::InceptionPlugin;
use fluidbg_operator::crd::state_store::StateStore;

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crds".to_string());
    let crds_dir = Path::new(&out_dir);
    std::fs::create_dir_all(crds_dir).expect("failed to create crds/ directory");

    let ip_crd = InceptionPlugin::crd();
    let ip_yaml = serde_yaml::to_string(&ip_crd).expect("failed to serialize InceptionPlugin CRD");
    std::fs::write(crds_dir.join("inception_plugin.yaml"), ip_yaml)
        .expect("failed to write InceptionPlugin CRD");

    let ss_crd = StateStore::crd();
    let ss_yaml = serde_yaml::to_string(&ss_crd).expect("failed to serialize StateStore CRD");
    std::fs::write(crds_dir.join("state_store.yaml"), ss_yaml)
        .expect("failed to write StateStore CRD");

    let bg_crd = BlueGreenDeployment::crd();
    let bg_yaml =
        serde_yaml::to_string(&bg_crd).expect("failed to serialize BlueGreenDeployment CRD");
    std::fs::write(crds_dir.join("blue_green_deployment.yaml"), bg_yaml)
        .expect("failed to write BlueGreenDeployment CRD");

    println!("CRD manifests written to {}", out_dir);
}
