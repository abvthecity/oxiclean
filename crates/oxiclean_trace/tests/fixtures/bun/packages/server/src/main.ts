import { version } from "@acme/shared";
import { log } from "logger";
import { port } from "./config";

log(`listening on ${port} (${version})`);
