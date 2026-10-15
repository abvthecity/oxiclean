import { S3 } from "mega-sdk";
import { s3 } from "mega-sdk/clients/s3";
import { helper } from "./helper";

export const clients = [S3, s3, helper];
