# Description
CLI tool to help you tag automatically your cards on wiki-masters.

```
Usage: wiki-masters-tag [OPTIONS] --email <EMAIL> --password <PASSWORD> <COMMAND>

Commands:
  retag-all  Allows to retag all your collection according to the newest config file. This will remove all previous tags
  tag-new    Retag only newest untagged cards according to the config file
  dry-run    Allow you to debug why a given card was tagged with a specific tag
  init       Needed to be run once to download the wikipedia database
  trade      Send all the cards with the given tag to a given user
  help       Print this message or the help of the given subcommand(s)

Options:
  -c, --config-file <CONFIG_FILE>                    [default: config.yml]
      --database-folder-path <DATABASE_FOLDER_PATH>  [default: ./wikipedia_database]
      --email <EMAIL>                                
      --password <PASSWORD>                          
  -h, --help                                         Print help
  -V, --version                                      Print version
```

## On first run
Use the `init` subcommand (do not forget to run in release) for the first run to download the wikipedia database.  
This will create a default `config.yml` file, do not forget to adapt it to your needs.

For reference about the migration:
```
Page migration complete.
  [00:07:44] [░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░] 13,947,743/13,947,743 (30,050.0995/s, 0s)
CategoryLinks migration complete.
  [00:08:58] [░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░] 73,700,174/73,700,174 (136,752.9357/s, 0s)
LinkTarget migration complete.
  [00:06:21] [░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░] 30,895,664/30,895,664 (80,887.2394/s, 0s)
```