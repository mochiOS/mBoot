#!/usr/bin/env perl

use strict;
use warnings;
use FindBin;
use lib $FindBin::Bin;
use HvConfig qw(read_hv_config);

@ARGV == 2 or die "usage: $0 CONFIG KEY\n";
my ($file, $key) = @ARGV;
my $config = read_hv_config($file);
if ($key eq 'domain_count') {
    print scalar(@{$config->{domains}}), "\n";
    exit 0;
}
if ($key eq 'system_image') {
    my ($system) = grep { $_->{role} eq 'system' } @{$config->{domains}};
    print "$system->{image}\n";
    exit 0;
}
exists $config->{$key} && ref($config->{$key}) eq ''
    or die "unknown scalar config key: $key\n";
print "$config->{$key}\n";
