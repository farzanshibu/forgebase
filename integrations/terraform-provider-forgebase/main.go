// Command terraform-provider-forgebase is the Terraform provider for ForgeBase.
package main

import (
	"context"
	"flag"
	"log"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/provider"
)

// version is set at build time (-ldflags "-X main.version=...").
var version = "dev"

func main() {
	var debug bool
	flag.BoolVar(&debug, "debug", false, "run the provider with support for debuggers such as delve")
	flag.Parse()

	err := providerserver.Serve(context.Background(), provider.New(version), providerserver.ServeOpts{
		Address: "registry.terraform.io/forgebase/forgebase",
		Debug:   debug,
	})
	if err != nil {
		log.Fatal(err.Error())
	}
}
